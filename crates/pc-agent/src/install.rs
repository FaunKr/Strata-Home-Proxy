//! The Strata folder as setup leaves it: run configs (`strata-<id>.json`),
//! their Chat settings, start scripts and logs, and each model's files in the
//! Strata data folder. Everything here knows file names, letter case, the
//! config format Strata writes and the engine flags the service changes;
//! callers deal in model ids and [`Model`] values.
//!
//! Strata comes in two layouts. The Strata Terminal keeps its own files
//! (`serve`, `data`) in its folder and the run configs in a Strata folder
//! beside it ([`crate::terminal`]); an older Strata folder from source (a
//! Git clone) holds both. The run configs have the same format in both.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;
use strata_pc_protocol::Pictures;

use crate::{
    sampling::{self, SamplingSupport, StrataSampling},
    terminal::Terminal,
};

/// The engine flag setup writes the context with.
const CONTEXT_FLAG: &str = "--max-context";
/// The flags setup adds past the trained context, each with one value.
const ROPE_FLAGS: [&str; 2] = ["--rope-scaling", "--rope-scale"];
/// The engine flag setup loads the speed projection's control vector with.
const PROJECTION_VECTOR_FLAG: &str = "--control-vector-scaled";
/// The flags setup writes after it, with their values.
const PROJECTION_FLAGS: [(&str, &[&str]); 3] = [
    ("--control-vector-layer-range", &["4", "44"]),
    ("--cvec-mode", &["project"]),
    ("--cvec-dir", &["per-layer"]),
];
/// Since Strata 8aaf188a: the args as setup last wrote them. A setup run
/// takes every option of `args` that differs from them for one set by hand.
const WRITTEN_KEY: &str = "args_written";
/// Since Strata 8aaf188a: the options set by hand, which Strata puts over
/// `args` at every start and every setup run.
const USER_KEY: &str = "user_args";
/// The engine's environment, which Strata puts over the GPU choice's at
/// every start (`serve/server.py`).
const ENV_KEY: &str = "env";
/// Environment variables the GPU choice in Strata's setup sets for the
/// engine, and its search path for the engine's libraries; Windows reads
/// names without case.
const REFUSED_ENV: [&str; 4] = [
    "PATH",
    "CUDA_VISIBLE_DEVICES",
    "HIP_VISIBLE_DEVICES",
    "CUDA_DEVICE_ORDER",
];
/// Strata's vector, in the Strata folder.
const PROJECTION_VECTOR: [&str; 3] = [
    "data",
    "experimental-speed-projection",
    "Qwen3.8-Flash-Next-experimental-speed-projection.gguf",
];
/// Strata's recovery from reasoning that repeats whole passages (off by
/// default; `"recover"` goes on with the low-effort instruction). The
/// service sets it in every model config.
const LOOP_RECOVERY_KEY: &str = "reasoning_loop_recovery";
const LOOP_RECOVERY: &str = "recover";
/// The config's defaults for requests without sampling fields.
const SAMPLING_KEY: &str = "sampling";

type ConfigFields = serde_json::Map<String, Value>;

/// In the service's state folder while a switch to the Strata Terminal
/// has not proven itself: the old Strata folder stays the service's Strata,
/// though the Terminal has a run config already.
pub const SWITCH_FILE: &str = "strata-switch.json";

/// The Strata folder and the data folder its models live in.
#[derive(Clone, Debug)]
pub struct Installation {
    /// The run configs, their Chat settings, start scripts and logs.
    dir: PathBuf,
    /// Strata's own files: `setup.py`, `serve`, `data`.
    code: PathBuf,
    data: PathBuf,
    kind: Kind,
}

/// Which layout Strata has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// The Strata Terminal.
    Terminal,
    /// An older Strata folder from source.
    Source,
}

impl From<Kind> for strata_pc_protocol::StrataInstall {
    fn from(kind: Kind) -> Self {
        match kind {
            Kind::Terminal => Self::Terminal,
            Kind::Source => Self::Source,
        }
    }
}

/// A model's config as read, with its Chat settings. Change it with the
/// setters and give it to [`Installation::save`]; keep a clone to roll back.
#[derive(Clone, Debug, PartialEq)]
pub struct Model {
    id: String,
    fields: ConfigFields,
    chat: Option<ConfigFields>,
    vector: PathBuf,
}

impl Installation {
    /// A Strata folder from source.
    pub fn new(strata_dir: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) -> Self {
        let dir = strata_dir.into();
        Self {
            code: dir.clone(),
            dir,
            data: data_dir.into(),
            kind: Kind::Source,
        }
    }

    /// The Strata Terminal's layout.
    pub fn terminal(terminal: &Terminal, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: terminal.install_dir(),
            code: terminal.root(),
            data: data_dir.into(),
            kind: Kind::Terminal,
        }
    }

    /// The service's Strata: the Strata Terminal, unless an older Strata
    /// folder is there and the Terminal has no run configs yet, or a switch
    /// to the Terminal is under way ([`SWITCH_FILE`]). A PC without Strata
    /// gets the Terminal.
    pub fn of(cfg: &crate::Config) -> Self {
        let terminal = Self::terminal(&Terminal::of(cfg), &cfg.strata_data_dir);
        let source = Self::old(cfg);
        let switching = cfg.state_dir().join(SWITCH_FILE).exists();
        if source.exists() && (switching || terminal.models().is_empty()) {
            source
        } else {
            terminal
        }
    }

    pub fn kind(&self) -> Kind {
        self.kind
    }

    /// The Strata folder from source in `strata_dir`, there or not.
    pub fn old(cfg: &crate::Config) -> Self {
        Self::new(&cfg.strata_dir, &cfg.strata_data_dir)
    }

    /// Whether this Strata folder from source is there: its setup or run
    /// configs.
    pub fn exists(&self) -> bool {
        self.code.join("setup.py").is_file() || !self.models().is_empty()
    }

    /// Whether Strata is installed here: setup's folder, or the Terminal.
    pub fn is_set_up(&self) -> bool {
        match self.kind {
            Kind::Source => self.code.join("setup.py").is_file(),
            Kind::Terminal => Terminal::program_in(&self.code).is_file(),
        }
    }

    /// The ids of the model configs, as their file names spell them.
    pub fn models(&self) -> Vec<String> {
        std::fs::read_dir(&self.dir)
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let id = name.strip_prefix("strata-")?.strip_suffix(".json")?;
                // `strata-<model>.shared-settings.json` belongs to a model;
                // a model id itself may hold dots (`deepseek-v4.1-flash-…`).
                (!id.is_empty() && !id.ends_with(".shared-settings")).then(|| id.to_string())
            })
            .collect()
    }

    /// Whether a model has a config, in any letter case.
    pub fn has(&self, id: &str) -> bool {
        self.models().iter().any(|m| m.eq_ignore_ascii_case(id))
    }

    /// Whether `id` can name a model's files: letters, digits, `.`, `_`
    /// and `-`, never `..`; a path never comes from a request.
    pub fn plain_id(id: &str) -> bool {
        !id.is_empty()
            && !id.contains("..")
            && id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
    }

    /// The name of a model's download folder, in the letter case setup gave
    /// it (the run config's file name is lower case), if there is one.
    pub fn download_name(&self, id: &str) -> Option<String> {
        std::fs::read_dir(self.data.join("models"))
            .into_iter()
            .flatten()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .find(|name| name.eq_ignore_ascii_case(id))
    }

    /// A model's config for changing it: fails if it cannot be read or is no
    /// model config. Chat settings that cannot be read count as none.
    pub fn model(&self, id: &str) -> Result<Model, String> {
        let path = self.config_path(id);
        let (_, fields) = read_config(&path)?
            .ok_or_else(|| format!("{} is not a model config", path.display()))?;
        Ok(self.with_chat(id, fields))
    }

    /// A model as Strata reads it: a config that cannot be read counts as
    /// one without settings, as do Chat settings.
    pub fn read_model(&self, id: &str) -> Model {
        let fields = read_config(&self.config_path(id))
            .ok()
            .flatten()
            .map(|(_, fields)| fields)
            .unwrap_or_default();
        self.with_chat(id, fields)
    }

    fn with_chat(&self, id: &str, fields: ConfigFields) -> Model {
        let path = self.chat_path(id);
        let chat = std::fs::read_to_string(&path).ok().and_then(|text| {
            serde_json::from_str::<ConfigFields>(&text)
                .inspect_err(|e| tracing::debug!("{} is not valid JSON: {e}", path.display()))
                .ok()
        });
        Model {
            id: id.to_string(),
            fields,
            chat,
            vector: self.vector_path(),
        }
    }

    /// Writes a model's config as Strata does. Without its own start script
    /// Strata starts the config touched last: the change keeps the time, so
    /// the same model stays active. Chat settings are only read, never written.
    pub fn save(&self, model: &Model) -> Result<(), String> {
        write_config(&self.config_path(&model.id), &model.fields)
    }

    /// When a model's config was last modified; `None` without one.
    pub fn modified(&self, id: &str) -> Option<std::time::SystemTime> {
        std::fs::metadata(self.config_path(id))
            .and_then(|m| m.modified())
            .ok()
    }

    /// Gives a model's config back the modification time `time` from
    /// [`Self::modified`], after setup wrote it anew: Strata without its own
    /// start script starts the config touched last. Nothing without a time.
    pub fn set_modified(&self, id: &str, time: Option<std::time::SystemTime>) {
        if let Some(time) = time {
            set_modified(&self.config_path(id), time);
        }
    }

    /// The Strata key in the model configs: the first config's in the order
    /// of their ids that has one. The service writes the same key into all.
    pub fn key(&self) -> Option<String> {
        let mut ids = self.models();
        ids.sort();
        ids.iter().find_map(|id| {
            let (_, fields) = read_config(&self.config_path(id)).ok()??;
            fields
                .get("api_key")
                .and_then(Value::as_str)
                .filter(|k| !k.is_empty())
                .map(String::from)
        })
    }

    /// Sets the key in every model config, keeping everything else and each
    /// file's modification time (Strata without its own start script starts
    /// the config touched last). A config that cannot be read is logged and
    /// left as it is, as Strata cannot start it either; if one cannot be
    /// written, none keeps the new key.
    pub fn set_key(&self, key: &str) -> Result<(), String> {
        let mut changed = Vec::new();
        for id in self.models() {
            let path = self.config_path(&id);
            let (text, mut fields) = match read_config(&path) {
                Ok(Some(config)) => config,
                Ok(None) => continue,
                Err(e) => {
                    tracing::warn!("No new Strata key in it: {e}");
                    continue;
                }
            };
            fields.insert("api_key".into(), key.into());
            changed.push((path, text, config_text(&fields)));
        }
        for (i, (path, _, new)) in changed.iter().enumerate() {
            if let Err(e) = write_keeping_time(path, new) {
                // Back to the old key, so all configs agree with the service.
                for (path, old, _) in &changed[..i] {
                    let _ = write_keeping_time(path, old);
                }
                return Err(e);
            }
        }
        Ok(())
    }

    /// Brings every model config to what the service keeps in it: loop
    /// recovery, and the `sampling` block as [`sampling::managed_block`] has
    /// it for this Strata, over a hand-set one; the block stays while
    /// Strata's version is unknown. Gives the ids whose config changed. One
    /// that cannot be read or written is logged and left as it is; the
    /// others still change.
    pub fn manage(&self) -> Vec<String> {
        let support = self.support();
        self.models()
            .into_iter()
            .filter(|id| {
                let path = self.config_path(id);
                let written = read_config(&path).and_then(|found| match found {
                    Some((_, mut fields)) => {
                        if manage_config(&mut fields, support) {
                            write_config(&path, &fields).map(|()| true)
                        } else {
                            Ok(false)
                        }
                    }
                    None => Ok(false),
                });
                written.unwrap_or_else(|e| {
                    tracing::warn!("Model config not managed: {e}");
                    false
                })
            })
            .collect()
    }

    /// Which sampling defaults this Strata applies; `None` while unknown.
    pub fn support(&self) -> Option<SamplingSupport> {
        sampling::support_of(&self.code)
    }

    /// Deletes what setup left for a model (`id` as the catalog spells it):
    /// its folders in the data folder, then its log, start script and config.
    /// Files other models share (`mtp`) stay.
    pub fn remove(&self, id: &str) -> Result<(), String> {
        self.model_paths(id).iter().try_for_each(|path| {
            let removed = if path.is_dir() {
                std::fs::remove_dir_all(path)
            } else {
                std::fs::remove_file(path)
            };
            match removed {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("{} not deleted: {e}", path.display()))
                }
                _ => Ok(()),
            }
        })
    }

    /// Deletes a model's run config with its Chat settings, start script
    /// and log; its files in the data folder stay.
    pub fn remove_config(&self, id: &str) -> Result<(), String> {
        let paths = [
            self.log_path(id),
            self.run_script(id),
            self.chat_path(id),
            self.config_path(id),
        ];
        paths
            .iter()
            .try_for_each(|path| match std::fs::remove_file(path) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    Err(format!("{} not deleted: {e}", path.display()))
                }
                _ => Ok(()),
            })
    }

    /// Copies a model's Chat settings from `from`, if it has some.
    pub fn adopt_chat_settings(&self, from: &Installation, id: &str) -> Result<(), String> {
        let source = from.chat_path(id);
        if !source.is_file() {
            return Ok(());
        }
        let target = self.chat_path(id);
        std::fs::copy(&source, &target)
            .map(drop)
            .map_err(|e| format!("{} not copied: {e}", source.display()))
    }

    /// Deletes the whole Strata folder, but never the Strata data folder,
    /// the folders in `keep` or the folders on the way to them, however
    /// their paths are spelled (letter case, short names, junctions).
    pub fn remove_folder(&self, keep: &[PathBuf]) -> Result<(), String> {
        let keep: Vec<PathBuf> = keep
            .iter()
            .chain([&self.data])
            .map(|k| same_spelling(k))
            .collect();
        if keep.iter().any(|k| same_spelling(&self.dir).starts_with(k)) {
            return Err(format!(
                "{} lies in a folder that stays and is not deleted",
                self.dir.display()
            ));
        }
        remove_except(&self.dir, &keep).map(drop)
    }

    fn model_paths(&self, id: &str) -> Vec<PathBuf> {
        vec![
            self.download_dir(id),
            self.pack_dir(id),
            self.log_path(id),
            self.run_script(id),
            // The config last: until it is gone, the model is listed and can be deleted again.
            self.config_path(id),
        ]
    }

    /// Bytes of a model's files in the data folder (`id` as the catalog spells it).
    pub fn disk_bytes(&self, id: &str) -> u64 {
        dir_size(&self.download_dir(id)) + dir_size(&self.pack_dir(id))
    }

    /// Bytes of a model's download so far (`id` as the catalog spells it).
    pub fn downloaded_bytes(&self, id: &str) -> u64 {
        dir_size(&self.download_dir(id))
    }

    /// Whether parts of a model's download are there already.
    pub fn partly_downloaded(&self, id: &str) -> bool {
        std::fs::read_dir(self.download_dir(id))
            .map(|mut entries| {
                entries.any(|e| e.is_ok_and(|e| e.file_name().to_string_lossy().contains(".gguf")))
            })
            .unwrap_or(false)
    }

    /// A model's start script, `run-<id>.bat`, as setup names it.
    pub fn run_script(&self, id: &str) -> PathBuf {
        self.dir.join(format!("run-{}.bat", lower(id)))
    }

    /// The model a start with `start_script` and `args` starts, by its run
    /// config's file name: the run config `strata serve` serves, or the
    /// model's own start script. A script that is no model's own
    /// (`START-HERE.bat` starts the model used last) gives the run config
    /// Strata touched last, as Strata picks it.
    pub fn active_model(&self, start_script: &Path, args: &[String]) -> Option<String> {
        let models = self.models();
        if let Some(config) = Terminal::served_config(args) {
            let name = config.file_name()?.to_string_lossy().to_lowercase();
            return models
                .iter()
                .find(|id| {
                    self.config_path(id)
                        .file_name()
                        .is_some_and(|n| *n == *name)
                })
                .cloned();
        }
        let script = start_script
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        models
            .iter()
            .find(|id| {
                self.run_script(id)
                    .file_name()
                    .is_some_and(|n| *n == *script)
            })
            .or_else(|| {
                models.iter().max_by_key(|id| {
                    std::fs::metadata(self.config_path(id))
                        .and_then(|m| m.modified())
                        .ok()
                })
            })
            .cloned()
    }

    /// A model's config file, for Strata's programs that take one.
    pub fn config_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("strata-{}.json", lower(id)))
    }

    /// A model's Chat settings from Strata's web app.
    fn chat_path(&self, id: &str) -> PathBuf {
        self.dir
            .join(format!("strata-{}.shared-settings.json", lower(id)))
    }

    fn log_path(&self, id: &str) -> PathBuf {
        self.dir.join(format!("strata-{}.log", lower(id)))
    }

    /// A model's download, in the catalog's letter case as setup names it.
    fn download_dir(&self, id: &str) -> PathBuf {
        self.data.join("models").join(id)
    }

    fn pack_dir(&self, id: &str) -> PathBuf {
        self.data.join("packs").join(lower(id))
    }

    /// Strata's control vector for the speed projection.
    fn vector_path(&self) -> PathBuf {
        PROJECTION_VECTOR
            .iter()
            .fold(self.code.clone(), |p, part| p.join(part))
    }
}

impl Model {
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The config's fields, as read or changed.
    pub fn fields(&self) -> &ConfigFields {
        &self.fields
    }

    fn args(&self) -> &[Value] {
        self.fields
            .get("args")
            .and_then(Value::as_array)
            .map_or(&[], Vec::as_slice)
    }

    fn args_mut(&mut self) -> &mut Vec<Value> {
        let args = self
            .fields
            .entry("args")
            .or_insert_with(|| Value::Array(Vec::new()));
        if !args.is_array() {
            *args = Value::Array(Vec::new());
        }
        args.as_array_mut().expect("args is an array")
    }

    /// The args Strata starts with: `user_args` over `args`, as Strata's
    /// `engine_args` puts them.
    fn started_args(&self) -> Vec<Value> {
        let user = self
            .fields
            .get(USER_KEY)
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |user| groups(user));
        let mut args: Vec<Value> = groups(self.args())
            .into_iter()
            .filter(|g| flag_of(g).is_empty() || !user.iter().any(|u| flag_of(u) == flag_of(g)))
            .flatten()
            .collect();
        args.extend(user.into_iter().flatten());
        args
    }

    /// The engine options set by hand, as shown outside the PC: every path
    /// cut to its file name. `None` when its Strata keeps none, which a
    /// config without `args_written` says.
    pub fn shareable_user_args(&self) -> Option<Vec<String>> {
        self.fields.get(WRITTEN_KEY)?;
        let user = self
            .fields
            .get(USER_KEY)
            .and_then(Value::as_array)
            .map_or(&[][..], Vec::as_slice);
        Some(shareable_args(user))
    }

    /// The args Strata starts with, as shown outside the PC.
    pub fn shareable_started_args(&self) -> Vec<String> {
        shareable_args(&self.started_args())
    }

    /// Whether its Strata keeps engine options set by hand: since Strata
    /// 8aaf188a, whose configs have `args_written`.
    pub fn keeps_user_args(&self) -> bool {
        self.fields.contains_key(WRITTEN_KEY)
    }

    /// Why `requested` cannot replace the engine options set by hand, if
    /// it cannot: it starts with a value, or adds or changes an option with
    /// a switch of its own or one the proxy needs ([`refused_group`]). Such
    /// an option Strata put there itself may come back unchanged.
    pub fn refused_user_args(&self, requested: &[String]) -> Option<String> {
        if let Some(first) = requested.first().filter(|a| !a.starts_with("--")) {
            return Some(format!(
                "Engine options start with an option (--name), not \"{first}\""
            ));
        }
        let kept = self.shareable_user_args().unwrap_or_default();
        let kept = groups(&kept.into_iter().map(Value::from).collect::<Vec<_>>());
        groups(&values(requested))
            .into_iter()
            .filter(|group| !kept.contains(group))
            .find_map(|group| refused_group(&group))
    }

    /// Replaces the engine options set by hand with `requested`, which
    /// comes as [`Self::shareable_user_args`] shows them; empty removes them.
    /// A group equal to how an old one shows keeps the old one's whole
    /// value, so a cut path stays whole; each old group serves once. A key
    /// passed as `--api-key`, which never shows, stays.
    pub fn set_user_args(&mut self, requested: &[String]) {
        let mut old = self
            .fields
            .get(USER_KEY)
            .and_then(Value::as_array)
            .map_or_else(Vec::new, |user| groups(user));
        let mut user = Vec::new();
        for group in groups(&values(requested)) {
            let shown: Vec<String> = group
                .iter()
                .filter_map(Value::as_str)
                .map(Into::into)
                .collect();
            match old.iter().position(|g| shareable_args(g) == shown) {
                Some(at) => user.extend(old.remove(at)),
                None => user.extend(group),
            }
        }
        user.extend(old.into_iter().filter(|g| is_key_group(g)).flatten());
        if user.is_empty() {
            self.fields.remove(USER_KEY);
        } else {
            self.fields.insert(USER_KEY.into(), Value::Array(user));
        }
    }

    /// The engine environment, as shown outside the PC: every path cut to
    /// its file name, without variables whose name says they hold a secret.
    pub fn shareable_env(&self) -> BTreeMap<String, String> {
        self.env()
            .filter(|(name, _)| !is_secret(name))
            .map(|(name, value)| (name.clone(), shown(value)))
            .collect()
    }

    fn env(&self) -> impl Iterator<Item = (&String, &Value)> {
        self.fields
            .get(ENV_KEY)
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
    }

    /// Why `requested` cannot replace the engine environment, if it cannot:
    /// a name that is not one, or one the GPU choice sets. Such a variable
    /// already there may come back unchanged.
    pub fn refused_env(&self, requested: &BTreeMap<String, String>) -> Option<String> {
        let kept = self.shareable_env();
        requested.iter().find_map(|(name, value)| {
            if !strata_pc_protocol::is_env_name(name) {
                Some(format!(
                    "\"{name}\" is not a variable name: letters, digits and _, not starting with a digit"
                ))
            } else if REFUSED_ENV.iter().any(|r| r.eq_ignore_ascii_case(name))
                && kept.get(name) != Some(value)
            {
                Some(format!(
                    "{name} is set by the GPU choice: change it in Strata's setup"
                ))
            } else {
                None
            }
        })
    }

    /// Replaces the engine environment with `requested`, which comes as
    /// [`Self::shareable_env`] shows it; empty removes it. A value equal to
    /// how the old one shows keeps the old one, so a cut path stays whole;
    /// a variable with a secret, which never shows, stays.
    pub fn set_env(&mut self, requested: &BTreeMap<String, String>) {
        let old: serde_json::Map<String, Value> =
            self.env().map(|(n, v)| (n.clone(), v.clone())).collect();
        let mut env: serde_json::Map<String, Value> = old
            .iter()
            .filter(|(name, _)| is_secret(name))
            .map(|(n, v)| (n.clone(), v.clone()))
            .collect();
        for (name, value) in requested {
            let kept = old.get(name).filter(|v| shown(v) == *value);
            env.insert(
                name.clone(),
                kept.cloned().unwrap_or_else(|| value.as_str().into()),
            );
        }
        if env.is_empty() {
            self.fields.remove(ENV_KEY);
        } else {
            self.fields.insert(ENV_KEY.into(), Value::Object(env));
        }
    }

    /// The context it starts with (`--max-context`), if the config says.
    pub fn context(&self) -> Option<u32> {
        let args = self.started_args();
        let at = args.iter().position(|v| v == CONTEXT_FLAG)?;
        args.get(at + 1)?.as_str()?.parse().ok()
    }

    /// Whether the config has a picture reader, and which one: no `vision`
    /// section is `off`, `vision.gpu` the GPU reader or the CPU one. `None`
    /// for a config that cannot be read, as [`Self::shareable_config`] says.
    pub fn picture_state(&self) -> Option<Pictures> {
        if self.fields.is_empty() {
            return None;
        }
        // Strata starts a reader for a `vision` section that is a non-empty
        // object (`if cfg.get("vision")` in serve/server.py).
        let vision = self
            .fields
            .get("vision")
            .and_then(Value::as_object)
            .filter(|vision| !vision.is_empty());
        Some(match vision {
            Some(vision) if vision.get("gpu") == Some(&Value::Bool(true)) => Pictures::Gpu,
            Some(_) => Pictures::Cpu,
            None => Pictures::Off,
        })
    }

    /// Whether the config has a picture reader: setup writes a `vision`
    /// section only when asked for `--vision yes`.
    pub fn pictures(&self) -> bool {
        matches!(self.picture_state(), Some(Pictures::Cpu | Pictures::Gpu))
    }

    /// Whether the config loads a control vector, Strata's or another one.
    pub fn speed_projection(&self) -> bool {
        self.started_args()
            .iter()
            .any(|v| v == PROJECTION_VECTOR_FLAG)
    }

    /// Recovery from repeated reasoning passages, as Strata reads it: `true`
    /// is `recover`, a missing or unknown value `off`.
    pub fn loop_recovery(&self) -> &'static str {
        match self.fields.get(LOOP_RECOVERY_KEY) {
            Some(Value::Bool(true)) => LOOP_RECOVERY,
            Some(v) if v == LOOP_RECOVERY => LOOP_RECOVERY,
            Some(v) if v == "stop" => "stop",
            _ => "off",
        }
    }

    /// Takes back the engine flags `before` had, keeping everything else in
    /// the config as it is now: a rollback undoes only its own change.
    pub fn restore_args(&mut self, before: &Model) {
        let args = before.args().to_vec();
        *self.args_mut() = args;
        for key in [WRITTEN_KEY, USER_KEY, ENV_KEY] {
            match before.fields.get(key) {
                Some(v) => self.fields.insert(key.into(), v.clone()),
                None => self.fields.remove(key),
            };
        }
    }

    /// Changes `args`, and `args_written` the same way, so that Strata does
    /// not take the change for one set by hand; the options `owned` leave
    /// `user_args`, where they would win over `args`. Neither key is added
    /// to a config of an older Strata that has none.
    fn change_args(&mut self, owned: &[&str], change: impl Fn(&mut Vec<Value>)) {
        change(self.args_mut());
        if let Some(Value::Array(written)) = self.fields.get_mut(WRITTEN_KEY) {
            change(written);
        }
        if let Some(Value::Array(user)) = self.fields.get_mut(USER_KEY) {
            let kept: Vec<Value> = groups(user)
                .into_iter()
                .filter(|g| !owned.contains(&flag_of(g)))
                .flatten()
                .collect();
            if kept.is_empty() {
                self.fields.remove(USER_KEY);
            } else {
                *user = kept;
            }
        }
    }

    /// The config to show outside the PC: without `api_key` and with every
    /// path cut to its file name, so neither the key nor the Windows user
    /// name leaves the PC; laid out as Strata writes it. `None` for a config
    /// that could not be read.
    pub fn shareable_config(&self) -> Option<String> {
        (!self.fields.is_empty()).then(|| shareable_text(&self.fields))
    }

    /// The Chat settings, cut like the config; `None` without them.
    pub fn shareable_chat(&self) -> Option<String> {
        self.chat.as_ref().map(shareable_text)
    }

    /// The sampling its config and Chat settings give it with this Strata.
    pub fn sampling(&self, support: SamplingSupport) -> StrataSampling {
        sampling::effective(support, &self.fields, self.chat.as_ref())
    }

    /// This `--max-context`, in its place, without the rope scaling setup
    /// adds past the trained length: every context the switch takes is
    /// within it. Everything setup derived from the old context stays.
    pub fn set_context(&mut self, context: u32) {
        let owned = [CONTEXT_FLAG, ROPE_FLAGS[0], ROPE_FLAGS[1]];
        self.change_args(&owned, |args| with_context(args, context));
    }

    /// Switches the speed projection with the flags setup writes for it;
    /// any control vector loaded before goes. Fails, changing nothing, when
    /// switching on and Strata's control vector is missing.
    pub fn set_speed_projection(&mut self, on: bool) -> Result<(), String> {
        if on && !self.vector.is_file() {
            return Err(format!(
                "The speed projection's control vector is missing: {}",
                self.vector.display()
            ));
        }
        let vector = format!("{}:1.0", self.vector.display());
        // Strata keeps a vector of the user's own (`--control-vector`) with
        // these in user_args; switching replaces any vector, as in args.
        let mut owned = vec![PROJECTION_VECTOR_FLAG, "--control-vector"];
        owned.extend(PROJECTION_FLAGS.iter().map(|(flag, _)| *flag));
        self.change_args(&owned, |args| {
            without_speed_projection(args);
            if on {
                args.push(PROJECTION_VECTOR_FLAG.into());
                args.push(vector.clone().into());
                for (flag, values) in PROJECTION_FLAGS {
                    args.push(flag.into());
                    args.extend(values.iter().map(|&v| Value::from(v)));
                }
            }
        });
        Ok(())
    }
}

/// `args` with this `--max-context` in its place, without rope scaling.
fn with_context(args: &mut Vec<Value>, context: u32) {
    let mut kept = Vec::with_capacity(args.len());
    let mut set = false;
    let mut drop_value = false;
    for arg in args.drain(..) {
        if std::mem::take(&mut drop_value) {
            continue;
        }
        if arg.as_str().is_some_and(|a| ROPE_FLAGS.contains(&a)) {
            drop_value = true;
        } else if arg == CONTEXT_FLAG {
            drop_value = true;
            if !set {
                kept.extend([arg, context.to_string().into()]);
                set = true;
            }
        } else {
            kept.push(arg);
        }
    }
    if !set {
        kept.extend([CONTEXT_FLAG.into(), context.to_string().into()]);
    }
    *args = kept;
}

/// A key passed as `--api-key` (also `--api-key=…`), with its value.
fn is_key_group(group: &[Value]) -> bool {
    let flag = flag_of(group);
    flag == "--api-key" || flag.starts_with("--api-key=")
}

/// Args as JSON values.
fn values(args: &[String]) -> Vec<Value> {
    args.iter().map(|a| Value::from(a.as_str())).collect()
}

/// Why this group of engine options cannot be set by hand, if it cannot:
/// its option is one the service switches itself, or one the proxy needs to
/// reach Strata.
fn refused_group(group: &[Value]) -> Option<String> {
    let flag = flag_of(group);
    let flag = flag.split_once('=').map_or(flag, |(flag, _)| flag);
    let switch = if flag == CONTEXT_FLAG || ROPE_FLAGS.contains(&flag) {
        "Change context"
    } else if flag == PROJECTION_VECTOR_FLAG
        || flag == "--control-vector"
        || PROJECTION_FLAGS.iter().any(|(f, _)| *f == flag)
    {
        "the speed projection switch"
    } else if ["--host", "--port", "--api-key"].contains(&flag) {
        return Some(format!(
            "{flag} is set by the PC service, which the proxy reaches Strata through"
        ));
    } else {
        return None;
    };
    Some(format!("{flag} has its own control: use {switch}"))
}

/// An argument list in Strata's groups (`strata_control/user_args.py`): an
/// option and the values after it, up to the next `--option`.
fn groups(args: &[Value]) -> Vec<Vec<Value>> {
    let mut out: Vec<Vec<Value>> = Vec::new();
    for arg in args {
        match out.last_mut() {
            Some(group) if !arg.as_str().is_some_and(|a| a.starts_with("--")) => {
                group.push(arg.clone())
            }
            _ => out.push(vec![arg.clone()]),
        }
    }
    out
}

/// A group's option; empty for values before the first one.
fn flag_of(group: &[Value]) -> &str {
    group
        .first()
        .and_then(Value::as_str)
        .filter(|a| a.starts_with("--"))
        .unwrap_or("")
}

/// Args as text with each path cut ([`shareable`]), without a key passed
/// as `--api-key` (also `--api-key=…`) and its value.
fn shareable_args(args: &[Value]) -> Vec<String> {
    groups(args)
        .into_iter()
        .filter(|g| !is_key_group(g))
        .flatten()
        .map(|arg| match shareable(&arg) {
            Value::String(text) => text,
            other => other.to_string(),
        })
        .collect()
}

/// A value as shown outside the PC: text with a path cut ([`shareable`]),
/// anything else as JSON.
fn shown(value: &Value) -> String {
    match shareable(value) {
        Value::String(text) => text,
        other => other.to_string(),
    }
}

/// `fields` as JSON text without secrets and with every path cut to its
/// last part, laid out as Strata writes it.
fn shareable_text(fields: &ConfigFields) -> String {
    config_text(&shareable_fields(fields))
}

/// `fields` without any field whose name says it holds a secret (`api_key`,
/// a token, a password), with paths cut ([`shareable`]).
fn shareable_fields(fields: &ConfigFields) -> ConfigFields {
    fields
        .iter()
        .filter(|(key, _)| !is_secret(key))
        .map(|(key, value)| (key.clone(), shareable(value)))
        .collect()
}

fn is_secret(key: &str) -> bool {
    let key = key.to_ascii_lowercase();
    ["key", "token", "secret", "password"]
        .iter()
        .any(|end| key.ends_with(end))
}

/// `value` with secrets left out and each path cut to its last part: setup
/// writes absolute paths (`exe`, the GGUF in `args`, `cwd`, `tokenizer`,
/// `log`, `lib_dirs`, `vision`). An option keeps its name
/// (`--x=D:\a\b.bin` → `--x=b.bin`); other text, a URL among it, stays.
fn shareable(value: &Value) -> Value {
    match value {
        Value::Object(fields) => Value::Object(shareable_fields(fields)),
        Value::Array(items) => Value::Array(items.iter().map(shareable).collect()),
        Value::String(text) => Value::String(match text.split_once('=') {
            Some((option, path)) if option.starts_with('-') && is_path(path) => {
                format!("{option}={}", last_part(path))
            }
            _ if is_path(text) => last_part(text),
            _ => text.clone(),
        }),
        other => other.clone(),
    }
}

/// An absolute or Windows path: `C:\…`, `C:/…`, `\\server\…`, `/…`, or
/// anything with a backslash.
fn is_path(text: &str) -> bool {
    let bytes = text.as_bytes();
    let drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    drive || text.starts_with('/') || text.contains('\\')
}

/// A path's last part; a user's own folder (`C:\Users\<name>`) gives `~`,
/// so the name stays on the PC.
fn last_part(path: &str) -> String {
    let parts: Vec<&str> = path.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    let Some(last) = parts.last() else {
        return String::new();
    };
    let user_folder = parts.len() >= 2 && parts[parts.len() - 2].eq_ignore_ascii_case("users");
    if user_folder {
        "~".into()
    } else {
        (*last).to_string()
    }
}

/// Setup writes lower-case file names whatever the catalog's case.
fn lower(id: &str) -> String {
    id.to_lowercase()
}

/// A model config with its text as read, or `None` for a `strata-*.json`
/// that is none: model configs have `exe` and an `args` array, as Strata's
/// own tools tell them apart.
fn read_config(path: &Path) -> Result<Option<(String, ConfigFields)>, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| format!("{} not readable: {e}", path.display()))?;
    let cfg = serde_json::from_str::<Value>(text.trim_start_matches('\u{feff}'))
        .map_err(|e| format!("{} is not valid JSON: {e}", path.display()))?;
    let Value::Object(fields) = cfg else {
        return Ok(None);
    };
    if !fields.contains_key("exe") || !fields.get("args").is_some_and(Value::is_array) {
        return Ok(None);
    }
    Ok(Some((text, fields)))
}

fn write_config(path: &Path, fields: &ConfigFields) -> Result<(), String> {
    write_keeping_time(path, &config_text(fields))
}

/// Sets a file's modification time; a failure is only logged.
fn set_modified(path: &Path, time: std::time::SystemTime) {
    let kept = std::fs::File::options()
        .write(true)
        .open(path)
        .and_then(|f| f.set_modified(time));
    if let Err(e) = kept {
        tracing::warn!("Kept no modification time on {}: {e}", path.display());
    }
}

/// Replaces the file's text and gives it back its modification time.
fn write_keeping_time(path: &Path, text: &str) -> Result<(), String> {
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    crate::write_replacing(path, text)
        .map_err(|e| format!("{} not written: {e}", path.display()))?;
    if let Some(time) = modified {
        set_modified(path, time);
    }
    Ok(())
}

/// A config as Strata writes it (`json.dumps(cfg, indent=1)`).
fn config_text(fields: &ConfigFields) -> String {
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(b" ");
    let mut ser = serde_json::Serializer::with_formatter(&mut out, formatter);
    serde::Serialize::serialize(fields, &mut ser).expect("config serializes");
    String::from_utf8(out).expect("JSON is UTF-8")
}

/// Whether managing changed the config.
fn manage_config(fields: &mut ConfigFields, support: Option<SamplingSupport>) -> bool {
    let before = fields.clone();
    fields.insert(LOOP_RECOVERY_KEY.into(), LOOP_RECOVERY.into());
    if let Some(support) = support {
        match sampling::managed_block(support, fields) {
            Some(block) => fields.insert(SAMPLING_KEY.into(), block),
            None => fields.remove(SAMPLING_KEY),
        };
    }
    *fields != before
}

/// Takes setup's speed projection flags out of `args`, each with its values,
/// whatever they are.
fn without_speed_projection(args: &mut Vec<Value>) {
    let mut kept = Vec::with_capacity(args.len());
    let mut values = 0;
    for arg in args.drain(..) {
        if let Some(n) = arg.as_str().and_then(projection_flag_values) {
            values = n;
        } else if values > 0 && !arg.as_str().is_some_and(|a| a.starts_with("--")) {
            values -= 1;
        } else {
            values = 0;
            kept.push(arg);
        }
    }
    *args = kept;
}

/// How many values one of setup's speed projection flags takes.
fn projection_flag_values(flag: &str) -> Option<usize> {
    if flag == PROJECTION_VECTOR_FLAG {
        return Some(1);
    }
    PROJECTION_FLAGS
        .iter()
        .find(|(f, _)| *f == flag)
        .map(|(_, values)| values.len())
}

/// A path spelled the one way two spellings of it compare equal: resolved
/// (short names, junctions, `subst`) where it exists, in lower case on
/// Windows, whose paths ignore letter case.
fn same_spelling(path: &Path) -> PathBuf {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    if cfg!(windows) {
        PathBuf::from(path.to_string_lossy().to_lowercase())
    } else {
        path
    }
}

/// Deletes `dir` with everything in it but `keep` (as [`same_spelling`]
/// gives them) and the folders on the way to them; gives whether `dir` is
/// still there.
fn remove_except(dir: &Path, keep: &[PathBuf]) -> Result<bool, String> {
    let not_deleted =
        |path: &Path, e: std::io::Error| format!("{} not deleted: {e}", path.display());
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(not_deleted(dir, e)),
    };
    let mut stays = false;
    for entry in entries {
        let entry = entry.map_err(|e| not_deleted(dir, e))?;
        let path = entry.path();
        let spelled = same_spelling(&path);
        if keep.contains(&spelled) {
            stays = true;
        } else if keep.iter().any(|k| k.starts_with(&spelled)) {
            stays |= remove_except(&path, keep)?;
        } else {
            let is_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            if is_dir {
                std::fs::remove_dir_all(&path)
            } else {
                std::fs::remove_file(&path)
            }
            .map_err(|e| not_deleted(&path, e))?;
        }
    }
    if !stays {
        std::fs::remove_dir(dir).map_err(|e| not_deleted(dir, e))?;
    }
    Ok(stays)
}

fn dir_size(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .filter_map(|e| e.ok())
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map_or(0, |m| m.len()),
            Err(_) => 0,
        })
        .sum()
}

/// A Strata folder as setup leaves it, for tests.
pub mod testing {
    use std::path::{Path, PathBuf};

    use serde_json::Value;

    use super::{ConfigFields, Installation, config_text};
    use crate::terminal::Terminal;

    /// Writes setup's files into a Strata folder and its data folder.
    pub struct SetupLeaves {
        install: Installation,
    }

    fn write(path: PathBuf, content: &str) {
        std::fs::create_dir_all(path.parent().expect("a file in a folder")).unwrap();
        std::fs::write(path, content).unwrap();
    }

    impl SetupLeaves {
        /// Setup's own files (`setup.py`, a Git checkout at this commit).
        pub fn new(strata_dir: &Path, data_dir: &Path, commit: &str) -> Self {
            write(strata_dir.join("setup.py"), "");
            write(strata_dir.join(".git").join("HEAD"), &format!("{commit}\n"));
            Self::at(strata_dir, data_dir)
        }

        /// The Strata Terminal's files as its install script leaves them in
        /// `home`, at this version.
        pub fn terminal(home: &Path, data_dir: &Path, version: &str) -> Self {
            Terminal::new(home).fake_install(version);
            Self::terminal_at(home, data_dir)
        }

        /// The Strata Terminal's folders as they are, to add to or look at.
        pub fn terminal_at(home: &Path, data_dir: &Path) -> Self {
            Self {
                install: Installation::terminal(&Terminal::new(home), data_dir),
            }
        }

        /// The folders as they are, to add to or look at.
        pub fn at(strata_dir: &Path, data_dir: &Path) -> Self {
            Self {
                install: Installation::new(strata_dir, data_dir),
            }
        }

        pub fn installation(&self) -> &Installation {
            &self.install
        }

        /// A model's config, written as Strata writes it, and its start script.
        pub fn model(&self, id: &str, config: Value) -> &Self {
            self.config(id, config);
            write(self.install.run_script(id), "");
            self
        }

        /// A model's config alone, written as Strata writes it; replaces one there.
        pub fn config(&self, id: &str, config: Value) -> &Self {
            let fields: ConfigFields = match config {
                Value::Object(fields) => fields,
                other => panic!("a config is an object: {other}"),
            };
            self.config_text(id, &config_text(&fields))
        }

        /// A model's config file with exactly this text, broken or not.
        pub fn config_text(&self, id: &str, text: &str) -> &Self {
            write(self.install.config_path(id), text);
            self
        }

        /// A model's Chat settings file with exactly this text.
        pub fn chat_settings(&self, id: &str, text: &str) -> &Self {
            write(self.install.chat_path(id), text);
            self
        }

        /// A model's log.
        pub fn log(&self, id: &str, text: &str) -> &Self {
            write(self.install.log_path(id), text);
            self
        }

        /// A file of a model's download (`id` as the catalog spells it).
        pub fn download(&self, id: &str, name: &str, content: &str) -> &Self {
            write(self.install.download_dir(id).join(name), content);
            self
        }

        /// A file in a model's pack, at `rel` inside it.
        pub fn pack(&self, id: &str, rel: &str, content: &str) -> &Self {
            write(self.install.pack_dir(id).join(rel), content);
            self
        }

        /// A file in the data folder that models share, at `rel`.
        pub fn shared(&self, rel: &str, content: &str) -> &Self {
            write(self.install.data.join(rel), content);
            self
        }

        /// Strata's control vector for the speed projection.
        pub fn speed_projection_vector(&self) -> &Self {
            write(self.vector(), "vector");
            self
        }

        /// Where Strata keeps the speed projection's control vector.
        pub fn vector(&self) -> PathBuf {
            self.install.vector_path()
        }

        /// A model config's modification time.
        pub fn modified(&self, id: &str) -> std::time::SystemTime {
            std::fs::metadata(self.install.config_path(id))
                .and_then(|m| m.modified())
                .unwrap()
        }

        /// A model's Chat settings as they are on disk.
        pub fn chat_text(&self, id: &str) -> String {
            std::fs::read_to_string(self.install.chat_path(id)).unwrap()
        }

        /// A model config's text as it is on disk.
        pub fn text(&self, id: &str) -> String {
            std::fs::read_to_string(self.install.config_path(id)).unwrap()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use serde_json::json;

    use super::{testing::SetupLeaves, *};

    /// A throwaway Strata folder and data folder.
    struct Folders(PathBuf);

    impl Folders {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("strata-install-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            Self(dir)
        }

        fn setup(&self) -> SetupLeaves {
            SetupLeaves::new(&self.0.join("Strata"), &self.0.join("data"), "0123456789")
        }
    }

    impl Drop for Folders {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn put(path: PathBuf, content: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    }

    #[test]
    fn removing_the_folder_keeps_a_folder_inside_however_it_is_spelled() {
        let f = Folders::new("remove-folder");
        let strata = f.0.join("Strata");
        let data = strata.join("models").join("data");
        let install = Installation::new(&strata, &data);
        put(strata.join("setup.py"), "");
        put(strata.join(".git").join("HEAD"), "0123456789\n");
        put(data.join("models").join("IQ3_XXS").join("a.gguf"), "0123");
        put(strata.join("models").join("other.txt"), "");
        let spelled = strata.join("models").join("..").join("models").join("kept");
        put(spelled.join("state.json"), "{}");

        install.remove_folder(&[spelled]).unwrap();
        assert!(data.join("models").join("IQ3_XXS").join("a.gguf").is_file());
        assert!(
            strata
                .join("models")
                .join("kept")
                .join("state.json")
                .is_file()
        );
        assert!(!strata.join("setup.py").exists());
        assert!(!strata.join(".git").exists());
        assert!(!strata.join("models").join("other.txt").exists());
    }

    #[test]
    fn a_strata_folder_inside_a_kept_one_is_not_deleted() {
        let f = Folders::new("remove-inside");
        let strata = f.0.join("data").join("Strata");
        put(strata.join("setup.py"), "");
        let install = Installation::new(&strata, f.0.join("data"));
        assert!(install.remove_folder(&[]).is_err());
        assert!(strata.join("setup.py").is_file());
    }

    fn config(args: Value) -> Value {
        json!({"exe": "C:/Strata/engine/strata.exe", "args": args, "port": 8080,
            "api_key": "key-0123456789abcdef"})
    }

    fn strata_knows(s: &SetupLeaves, text: &str) {
        let server = s.installation().dir.join("serve").join("server.py");
        std::fs::create_dir_all(server.parent().unwrap()).unwrap();
        std::fs::write(server, text).unwrap();
    }

    /// A run config as setup writes it (`setup.py` at 011c9e6, step 7:
    /// `cfg = {"exe": engine_exe(eng), "args": args, "cwd": str(ROOT), …}`),
    /// with the paths of a PC whose Windows user is `Faun`.
    const SETUP_CONFIG: &str = r#"{
 "exe": "C:\\Users\\Faun\\AppData\\Local\\Strata\\terminal\\current\\engine\\strata.exe",
 "args": [
  "--native",
  "D:\\Strata-data\\models\\IQ3_XXS\\Qwen3.8-Flash-Next-IQ3_XXS.gguf",
  "--max-context",
  "65536",
  "--prefill",
  "auto",
  "--mtp",
  "2",
  "--pool-workers",
  "12"
 ],
 "cwd": "C:\\Users\\Faun\\AppData\\Local\\Strata\\terminal\\current",
 "tokenizer": "D:\\Strata-data\\packs\\iq3_xxs\\tokenizer",
 "model_name": "qwen3.8-flash-next-iq3_xxs",
 "arch": "qwen35moe",
 "log": "C:\\Users\\Faun\\AppData\\Local\\Strata\\install\\strata-iq3_xxs.log",
 "lib_dirs": [
  "C:\\Users\\Faun\\AppData\\Local\\Strata\\terminal\\current\\engine"
 ],
 "port": 8080,
 "host": "0.0.0.0",
 "api_key": "key-0123456789abcdef",
 "vision": {
  "exe": "C:\\Users\\Faun\\AppData\\Local\\Strata\\terminal\\current\\engine\\strata-vision.exe",
  "mmproj": "D:\\Strata-data\\models\\IQ3_XXS\\mmproj.gguf",
  "model": "D:\\Strata-data\\models\\IQ3_XXS\\Qwen3.8-Flash-Next-IQ3_XXS.gguf",
  "gpu": true,
  "max_tokens": 1024
 }
}"#;

    #[test]
    fn the_shareable_config_has_no_key_and_no_folders() {
        let f = Folders::new("shareable");
        let s = f.setup();
        s.config_text("IQ3_XXS", SETUP_CONFIG);
        let text = s
            .installation()
            .read_model("IQ3_XXS")
            .shareable_config()
            .unwrap();
        let config: Value = serde_json::from_str(&text).unwrap();
        assert!(!text.contains("api_key"), "{text}");
        assert!(!text.contains("key-0123456789abcdef"), "{text}");
        assert!(!text.contains("Faun"), "{text}");
        assert!(!text.contains('\\') && !text.contains('/'), "{text}");
        assert_eq!(config["exe"], "strata.exe");
        assert_eq!(config["args"][1], "Qwen3.8-Flash-Next-IQ3_XXS.gguf");
        assert_eq!(config["args"][3], "65536");
        assert_eq!(config["cwd"], "current");
        assert_eq!(config["lib_dirs"][0], "engine");
        assert_eq!(config["vision"]["mmproj"], "mmproj.gguf");
        assert_eq!(config["vision"]["max_tokens"], 1024);
        assert_eq!(config["port"], 8080);
        assert_eq!(config["model_name"], "qwen3.8-flash-next-iq3_xxs");
    }

    #[test]
    fn only_paths_are_cut_and_no_secret_or_user_name_stays() {
        let fields: ConfigFields = serde_json::from_value(json!({
            "cwd": "C:\\Users\\Faun\\",
            "args": ["--kv-dir=D:\\x\\cache", "--prefill", "auto"],
            "url": "http://pc:8080/v1",
            "system_prompt": "Answer briefly and/or in German",
            "telemetry_read_key": "secret-1",
            "hf_token": "secret-2",
            "max_tokens": 4096,
            "top_k": 20
        }))
        .unwrap();
        let cut = shareable_fields(&fields);
        assert_eq!(
            Value::Object(cut),
            json!({
                "cwd": "~",
                "args": ["--kv-dir=cache", "--prefill", "auto"],
                "url": "http://pc:8080/v1",
                "system_prompt": "Answer briefly and/or in German",
                "max_tokens": 4096,
                "top_k": 20
            })
        );
    }

    #[test]
    fn chat_settings_are_shared_only_when_there_are_some() {
        let f = Folders::new("shareable-chat");
        let s = f.setup();
        s.config_text("IQ3_XXS", SETUP_CONFIG);
        s.config_text("swift-IQ2_XS", SETUP_CONFIG);
        s.chat_settings("IQ3_XXS", r#"{"temperature": 0.6, "top_k": 20}"#);
        let install = s.installation();
        let chat = install.read_model("IQ3_XXS").shareable_chat().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&chat).unwrap(),
            json!({"temperature": 0.6, "top_k": 20})
        );
        assert_eq!(install.read_model("swift-IQ2_XS").shareable_chat(), None);
    }

    #[test]
    fn lists_model_configs_but_not_chat_settings() {
        let f = Folders::new("list");
        let s = f.setup();
        s.model("iq3_xxs", config(json!([])))
            .chat_settings("iq3_xxs", "{}");
        let mut models = s.installation().models();
        models.sort();
        assert_eq!(models, ["iq3_xxs"]);
        assert!(s.installation().has("IQ3_XXS"));
        assert!(s.installation().is_set_up());
    }

    #[test]
    fn reads_a_models_settings_in_any_letter_case() {
        let f = Folders::new("read");
        let s = f.setup();
        s.model(
            "coder-iq1_m",
            json!({"exe": "strata.exe",
                "args": ["--native", "b.gguf", "--max-context", "65536",
                    "--control-vector-scaled", "other.gguf:1.0"],
                "reasoning_loop_recovery": true}),
        );
        let model = s.installation().model("coder-IQ1_M").unwrap();
        assert_eq!(model.context(), Some(65536));
        assert!(model.speed_projection());
        assert_eq!(model.loop_recovery(), "recover");
        assert_eq!(model.id(), "coder-IQ1_M");
    }

    #[test]
    fn loop_recovery_reads_as_strata_does() {
        let f = Folders::new("loop");
        let s = f.setup();
        for (value, read) in [
            (json!("stop"), "stop"),
            (json!("recover"), "recover"),
            (json!(false), "off"),
            (json!("weird"), "off"),
        ] {
            s.config(
                "m",
                json!({"exe": "s", "args": [], "reasoning_loop_recovery": value}),
            );
            assert_eq!(s.installation().model("m").unwrap().loop_recovery(), read);
        }
        s.config("m", json!({"exe": "s", "args": []}));
        assert_eq!(s.installation().model("m").unwrap().loop_recovery(), "off");
    }

    #[test]
    fn a_config_with_a_bom_reads() {
        let f = Folders::new("bom");
        let s = f.setup();
        s.config_text(
            "m",
            "\u{feff}{\"exe\": \"s\", \"args\": [\"--max-context\", \"32768\"]}",
        );
        assert_eq!(s.installation().model("m").unwrap().context(), Some(32768));
    }

    #[test]
    fn a_config_that_is_no_model_config_fails_to_change_and_reads_empty() {
        let f = Folders::new("nomodel");
        let s = f.setup();
        s.config_text("notes", r#"{"what": "not a model"}"#)
            .config_text("broken", "{ broken");
        let install = s.installation();
        let err = install.model("notes").unwrap_err();
        assert!(err.contains("is not a model config"), "{err}");
        let err = install.model("broken").unwrap_err();
        assert!(err.contains("is not valid JSON"), "{err}");
        let empty = install.read_model("broken");
        assert_eq!(empty.context(), None);
        assert!(!empty.speed_projection());
        assert_eq!(empty.loop_recovery(), "off");
    }

    #[test]
    fn chat_settings_count_for_sampling_and_unreadable_ones_as_none() {
        let f = Folders::new("chat");
        let s = f.setup();
        s.model("a", config(json!([])))
            .chat_settings("a", r#"{"temperature": 0.3}"#)
            .model("b", config(json!([])))
            .chat_settings("b", "{ broken");
        let temperature = |id: &str| {
            let sampling = s
                .installation()
                .read_model(id)
                .sampling(SamplingSupport::ModelCard);
            serde_json::to_value(sampling).unwrap()["thinking"][0].clone()
        };
        assert_eq!(temperature("a")["value"], 0.3);
        assert_eq!(temperature("a")["source"], "chat_settings");
        assert_eq!(temperature("b")["source"], "model_card");
    }

    #[test]
    fn setting_the_context_replaces_it_and_drops_rope_scaling() {
        let f = Folders::new("context");
        let s = f.setup();
        s.model(
            "m",
            config(json!([
                "--native",
                "a.gguf",
                "--max-context",
                "524288",
                "--rope-scaling",
                "yarn",
                "--rope-scale",
                "2",
                "--threads",
                "8"
            ])),
        );
        let mut model = s.installation().model("m").unwrap();
        model.set_context(131072);
        assert_eq!(
            model.fields()["args"],
            json!([
                "--native",
                "a.gguf",
                "--max-context",
                "131072",
                "--threads",
                "8"
            ])
        );
        let mut bare = s.installation().model("m").unwrap();
        bare.fields.insert("args".into(), json!(["a.gguf"]));
        bare.set_context(65536);
        assert_eq!(
            bare.fields()["args"],
            json!(["a.gguf", "--max-context", "65536"])
        );
    }

    #[test]
    fn saving_keeps_the_rest_strata_formatting_and_the_modification_time() {
        let f = Folders::new("save");
        let s = f.setup();
        s.model("m", config(json!(["--max-context", "65536"])));
        let before = s.modified("m");
        std::thread::sleep(Duration::from_millis(20));
        let install = s.installation();
        let mut model = install.model("m").unwrap();
        model.set_context(32768);
        install.save(&model).unwrap();
        assert_eq!(s.modified("m"), before);
        let text = s.text("m");
        assert!(text.starts_with("{\n \""), "Strata's indent of one: {text}");
        let saved = install.model("m").unwrap();
        assert_eq!(saved.context(), Some(32768));
        assert_eq!(saved.fields()["port"], 8080);
        assert_eq!(saved.fields()["api_key"], "key-0123456789abcdef");
    }

    #[test]
    fn saving_the_old_value_rolls_back() {
        let f = Folders::new("rollback");
        let s = f.setup();
        s.model("m", config(json!(["--max-context", "65536"])));
        let install = s.installation();
        let old = install.model("m").unwrap();
        let mut new = old.clone();
        new.set_context(262144);
        install.save(&new).unwrap();
        install.save(&old).unwrap();
        assert_eq!(install.model("m").unwrap(), old);
    }

    #[test]
    fn speed_projection_needs_its_vector_and_replaces_any_other() {
        let f = Folders::new("esp");
        let s = f.setup();
        s.model(
            "m",
            config(json!([
                "a.gguf",
                "--control-vector-scaled",
                "other.gguf:0.5",
                "--cvec-mode",
                "add",
                "--threads",
                "8"
            ])),
        );
        let install = s.installation();
        let mut model = install.model("m").unwrap();
        let err = model.set_speed_projection(true).unwrap_err();
        assert!(err.contains("control vector is missing"), "{err}");
        assert_eq!(model, install.model("m").unwrap(), "nothing changed");

        s.speed_projection_vector();
        model.set_speed_projection(true).unwrap();
        let vector = format!("{}:1.0", s.vector().display());
        assert_eq!(
            model.fields()["args"],
            json!([
                "a.gguf",
                "--threads",
                "8",
                "--control-vector-scaled",
                vector,
                "--control-vector-layer-range",
                "4",
                "44",
                "--cvec-mode",
                "project",
                "--cvec-dir",
                "per-layer"
            ])
        );
        model.set_speed_projection(false).unwrap();
        assert_eq!(model.fields()["args"], json!(["a.gguf", "--threads", "8"]));
        assert!(!model.speed_projection());
    }

    #[test]
    fn picture_reading_is_the_vision_section_of_the_config() {
        let f = Folders::new("vision");
        let s = f.setup();
        // Setup's `VR.vision_section` for the GPU reader and the CPU one.
        s.config(
            "gpu",
            json!({"exe": "strata.exe", "args": [], "vision": {
                "exe": "C:\\Strata\\engine\\strata-vision-hip.exe",
                "mmproj": "D:\\models\\mmproj-Qwen3.8-Flash-Next-BF16.gguf",
                "model": "model.gguf", "gpu": true, "max_tokens": 1024,
                "cpu_exe": "C:\\Strata\\engine\\strata-vision.exe"
            }}),
        );
        s.config(
            "cpu",
            json!({"exe": "strata.exe", "args": [], "vision": {
                "exe": "C:\\Strata\\engine\\strata-vision.exe",
                "mmproj": "mmproj.gguf",
                "model": "model.gguf", "gpu": false, "max_tokens": 300, "threads": 8
            }}),
        );
        s.config("plain", config(json!(["--native"])));
        s.config(
            "empty",
            json!({"exe": "strata.exe", "args": [], "vision": {}}),
        );
        s.config(
            "null",
            json!({"exe": "strata.exe", "args": [], "vision": null}),
        );
        let install = s.installation();
        for id in ["empty", "null"] {
            assert!(!install.read_model(id).pictures(), "{id}");
            assert_eq!(
                install.read_model(id).picture_state(),
                Some(Pictures::Off),
                "{id}"
            );
        }
        assert!(install.read_model("gpu").pictures());
        assert!(install.read_model("cpu").pictures());
        assert!(!install.read_model("plain").pictures());
    }

    #[test]
    fn a_new_key_goes_into_every_readable_config() {
        let f = Folders::new("key");
        let s = f.setup();
        s.model("a", config(json!([])))
            .model("b", config(json!([])));
        let install = s.installation();
        install.set_key("new-key-0123456789").unwrap();
        for id in ["a", "b"] {
            assert_eq!(
                install.model(id).unwrap().fields()["api_key"],
                "new-key-0123456789"
            );
        }
        assert_eq!(install.key().as_deref(), Some("new-key-0123456789"));
        s.config_text("c", "{ broken");
        install.set_key("third-key-0123456789").unwrap();
        for id in ["a", "b"] {
            assert_eq!(
                install.model(id).unwrap().fields()["api_key"],
                "third-key-0123456789"
            );
        }
        assert_eq!(
            std::fs::read_to_string(install.config_path("c")).unwrap(),
            "{ broken"
        );
    }

    #[test]
    fn managing_sets_loop_recovery_and_the_block_and_names_what_changed() {
        let f = Folders::new("manage");
        let s = f.setup();
        s.model("a", config(json!([])))
            .model(
                "b",
                json!({"exe": "s", "args": [], "arch": "qwen35moe",
                "reasoning_loop_recovery": "recover", "sampling": {"temperature": 0.2}}),
            )
            .config_text("broken", "{ broken");
        let install = s.installation();

        // Strata's version unknown: the hand-set block stays.
        let mut changed = install.manage();
        changed.sort();
        assert_eq!(changed, ["a"]);
        assert_eq!(
            install.model("b").unwrap().fields()["sampling"],
            json!({"temperature": 0.2})
        );
        assert_eq!(s.text("broken"), "{ broken");

        strata_knows(&s, "def sampling_defaults_from_config(cfg):\n");
        let mut changed = install.manage();
        changed.sort();
        assert_eq!(changed, ["a", "b"]);
        assert_eq!(
            install.model("b").unwrap().fields()["sampling"],
            json!({"temperature": 1.0, "top_p": 0.95, "top_k": 20, "presence_penalty": 1.5})
        );
        assert!(install.manage().is_empty(), "nothing left to change");

        strata_knows(
            &s,
            "def sampling_defaults_from_config(cfg):\nMODEL_CARD_SAMPLING = {}\n",
        );
        install.manage();
        assert!(
            install
                .model("b")
                .unwrap()
                .fields()
                .get("sampling")
                .is_none()
        );
        assert_eq!(install.model("a").unwrap().loop_recovery(), "recover");
    }

    #[test]
    fn removing_a_model_takes_its_files_config_last_and_keeps_shared_ones() {
        let f = Folders::new("remove");
        let s = f.setup();
        s.model("coder-iq1_m", config(json!([])))
            .log("coder-iq1_m", "log")
            .download("coder-IQ1_M", "b.gguf", "01234")
            .pack("coder-iq1_m", "tokenizer/vocab.json", "abc")
            .shared("mtp/rt/experts.bin", "shared")
            .model("iq3_xxs", config(json!([])));
        let install = s.installation();
        assert_eq!(install.disk_bytes("coder-IQ1_M"), 8);
        install.remove("coder-IQ1_M").unwrap();
        assert_eq!(install.models(), ["iq3_xxs"]);
        assert_eq!(install.disk_bytes("coder-IQ1_M"), 0);
        assert!(!install.run_script("coder-IQ1_M").exists());
        assert!(install.data.join("mtp/rt/experts.bin").exists());
        assert!(install.paths_left("coder-IQ1_M").is_empty());
        install.remove("coder-IQ1_M").unwrap();
        // Until the config is gone the model is listed, so a delete that
        // stopped halfway can run again.
        assert_eq!(
            install.model_paths("coder-IQ1_M").last(),
            Some(&install.config_path("coder-IQ1_M"))
        );
    }

    #[test]
    fn restoring_args_keeps_what_changed_in_between() {
        let f = Folders::new("restore");
        let s = f.setup();
        s.model("m", config(json!(["--max-context", "65536"])));
        let install = s.installation();
        let before = install.model("m").unwrap();
        let mut switched = before.clone();
        switched.set_context(131072);
        install.save(&switched).unwrap();
        install.set_key("other-key-0123456789").unwrap();
        let mut now = install.model("m").unwrap();
        now.restore_args(&before);
        install.save(&now).unwrap();
        let rolled_back = install.model("m").unwrap();
        assert_eq!(rolled_back.context(), Some(65536));
        assert_eq!(rolled_back.fields()["api_key"], "other-key-0123456789");
    }

    #[test]
    fn the_active_model_is_the_start_scripts_or_the_config_touched_last() {
        let f = Folders::new("active");
        let s = f.setup();
        s.model("coder-iq1_m", config(json!([])));
        std::thread::sleep(Duration::from_millis(20));
        s.model("iq3_xxs", config(json!([])));
        let install = s.installation();
        let script = install.run_script("CODER-IQ1_M");
        assert_eq!(
            install.active_model(&script, &[]).as_deref(),
            Some("coder-iq1_m")
        );
        let start_here = install.dir.join("START-HERE.bat");
        assert_eq!(
            install.active_model(&start_here, &[]).as_deref(),
            Some("iq3_xxs")
        );
        let served = Terminal::serve_args(&install.config_path("CODER-IQ1_M"), "0.0.0.0", "8080");
        assert_eq!(
            install
                .active_model(Path::new("strata.cmd"), &served)
                .as_deref(),
            Some("coder-iq1_m")
        );
    }

    #[test]
    fn partly_downloaded_models_are_told_by_their_gguf_parts() {
        let f = Folders::new("download");
        let s = f.setup();
        s.download("IQ3_XXS", "a-00001-of-00002.gguf", "0123");
        let install = s.installation();
        assert!(install.partly_downloaded("IQ3_XXS"));
        assert!(!install.partly_downloaded("coder-IQ1_M"));
        assert_eq!(install.downloaded_bytes("IQ3_XXS"), 4);
    }

    impl Installation {
        fn paths_left(&self, id: &str) -> Vec<PathBuf> {
            self.model_paths(id)
                .into_iter()
                .filter(|p| p.exists())
                .collect()
        }
    }
}
