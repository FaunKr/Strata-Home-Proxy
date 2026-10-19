//! The Strata-Ordner as setup leaves it: model configs (`strata-<id>.json`),
//! their Chat settings, start scripts and logs, and each model's files in the
//! Strata data folder. Everything here knows file names, letter case, the
//! config format Strata writes and the engine flags the service changes;
//! callers deal in model ids and [`Model`] values.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::sampling::{self, ModelSampling, Support};

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
/// Strata's vector, in the Strata-Ordner.
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

/// The Strata-Ordner and the data folder its models live in.
#[derive(Clone, Debug)]
pub struct Installation {
    dir: PathBuf,
    data: PathBuf,
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
    pub fn new(strata_dir: impl Into<PathBuf>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: strata_dir.into(),
            data: data_dir.into(),
        }
    }

    pub fn of(cfg: &crate::Config) -> Self {
        Self::new(&cfg.strata_dir, &cfg.strata_data_dir)
    }

    /// Whether setup has run here.
    pub fn is_set_up(&self) -> bool {
        self.dir.join("setup.py").is_file()
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
                // `strata-<model>.shared-settings.json` belongs to a model.
                (!id.is_empty() && !id.contains('.')).then(|| id.to_string())
            })
            .collect()
    }

    /// Whether a model has a config, in any letter case.
    pub fn has(&self, id: &str) -> bool {
        self.models().iter().any(|m| m.eq_ignore_ascii_case(id))
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

    /// Sets the key in every model config, keeping everything else; if one
    /// cannot be read or written, none keeps the new key.
    pub fn set_key(&self, key: &str) -> Result<(), String> {
        // Every config is read first: one that does not parse stops the change
        // before any has the new key.
        let mut changed = Vec::new();
        for id in self.models() {
            let path = self.config_path(&id);
            let Some((text, mut fields)) = read_config(&path)? else {
                continue;
            };
            fields.insert("api_key".into(), key.into());
            changed.push((path, text, config_text(&fields)));
        }
        for (i, (path, _, new)) in changed.iter().enumerate() {
            if let Err(e) = crate::write_replacing(path, new) {
                // Back to the old key, so all configs agree with the service.
                for (path, old, _) in &changed[..i] {
                    let _ = crate::write_replacing(path, old);
                }
                return Err(format!("{} not written: {e}", path.display()));
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
    pub fn support(&self) -> Option<Support> {
        Support::of(&self.dir)
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

    /// The model `start_script` starts, as its config file names it. A script
    /// that is no model's own (`START-HERE.bat` starts the model used last)
    /// gives the config Strata touched last, as Strata picks it.
    pub fn active_model(&self, start_script: &Path) -> Option<String> {
        let script = start_script
            .file_name()
            .map(|n| n.to_string_lossy().to_lowercase())
            .unwrap_or_default();
        let models = self.models();
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
            .fold(self.dir.clone(), |p, part| p.join(part))
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

    /// The context it starts with (`--max-context`), if the config says.
    pub fn context(&self) -> Option<u32> {
        let args = self.args();
        let at = args.iter().position(|v| v == CONTEXT_FLAG)?;
        args.get(at + 1)?.as_str()?.parse().ok()
    }

    /// Whether the config loads a control vector, Strata's or another one.
    pub fn speed_projection(&self) -> bool {
        self.args().iter().any(|v| v == PROJECTION_VECTOR_FLAG)
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
    }

    /// The sampling its config and Chat settings give it with this Strata.
    pub fn sampling(&self, support: Support) -> ModelSampling {
        sampling::effective(support, &self.fields, self.chat.as_ref())
    }

    /// This `--max-context`, in its place, without the rope scaling setup
    /// adds past the trained length: every context the switch takes is
    /// within it. Everything setup derived from the old context stays.
    pub fn set_context(&mut self, context: u32) {
        let args = self.args_mut();
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
        let args = self.args_mut();
        without_speed_projection(args);
        if on {
            args.push(PROJECTION_VECTOR_FLAG.into());
            args.push(vector.into());
            for (flag, values) in PROJECTION_FLAGS {
                args.push(flag.into());
                args.extend(values.iter().map(|&v| Value::from(v)));
            }
        }
        Ok(())
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
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    crate::write_replacing(path, &config_text(fields))
        .map_err(|e| format!("{} not written: {e}", path.display()))?;
    if let Some(time) = modified {
        let kept = std::fs::File::options()
            .write(true)
            .open(path)
            .and_then(|f| f.set_modified(time));
        if let Err(e) = kept {
            tracing::warn!("Kept no modification time on {}: {e}", path.display());
        }
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
fn manage_config(fields: &mut ConfigFields, support: Option<Support>) -> bool {
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

/// A Strata-Ordner as setup leaves it, for tests.
pub mod testing {
    use std::path::{Path, PathBuf};

    use serde_json::Value;

    use super::{ConfigFields, Installation, config_text};

    /// Writes setup's files into a Strata-Ordner and its data folder.
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

    /// A throwaway Strata-Ordner and data folder.
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

    fn config(args: Value) -> Value {
        json!({"exe": "C:/Strata/engine/strata.exe", "args": args, "port": 8080,
            "api_key": "key-0123456789abcdef"})
    }

    fn strata_knows(s: &SetupLeaves, text: &str) {
        let server = s.installation().dir.join("serve").join("server.py");
        std::fs::create_dir_all(server.parent().unwrap()).unwrap();
        std::fs::write(server, text).unwrap();
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
            .config_text("broken", "{ kaputt");
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
            .chat_settings("b", "{ kaputt");
        let temperature = |id: &str| {
            let sampling = s.installation().read_model(id).sampling(Support::ModelCard);
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
    fn a_new_key_goes_into_every_config_or_none() {
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
        s.config_text("c", "{ kaputt");
        assert!(install.set_key("third-key-0123456789").is_err());
        for id in ["a", "b"] {
            assert_eq!(
                install.model(id).unwrap().fields()["api_key"],
                "new-key-0123456789"
            );
        }
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
            .config_text("broken", "{ kaputt");
        let install = s.installation();

        // Strata's version unknown: the hand-set block stays.
        let mut changed = install.manage();
        changed.sort();
        assert_eq!(changed, ["a"]);
        assert_eq!(
            install.model("b").unwrap().fields()["sampling"],
            json!({"temperature": 0.2})
        );
        assert_eq!(s.text("broken"), "{ kaputt");

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
            install.active_model(&script).as_deref(),
            Some("coder-iq1_m")
        );
        let start_here = install.dir.join("START-HERE.bat");
        assert_eq!(
            install.active_model(&start_here).as_deref(),
            Some("iq3_xxs")
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
