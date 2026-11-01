//! Learned expert profiles: which of a model's profiles Strata starts it
//! with, which one learns, and the run config keys for both.
//!
//! The profiles live in `strata_home\expert-profiles\<model>\<name>.bin`,
//! outside the Control and engine folders that Strata updates delete. A
//! learned profile that does not learn is `--expert-profile <its file>` in
//! the run config's args; a learning one is also `expert_profile_save` (with
//! `expert_profile_save_every` 5), and Strata's own start puts the saved
//! file in place of `--expert-profile` once it exists. *Standard* is the
//! file setup wrote, in the `data` folder of the run config's `cwd`.
//!
//! This module's state file says which profile of each model learns and
//! whether learning is held; the run configs follow it ([`manage`]). A run
//! config with an expert profile or its saving set by hand is left alone.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Mutex,
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};
use strata_pc_protocol::{
    ExpertLearning, ExpertProfile, ExpertProfiles, STANDARD_PROFILE, profile_name_problem,
};

use crate::install::{EXPERT_PROFILE_FLAG, EXPERT_SAVE_FLAG, Installation, Kind, Model};

/// In the service's state folder.
pub const STATE_FILE: &str = "expert-profiles.json";
/// Below `strata_home`.
pub const FOLDER: &str = "expert-profiles";
/// Minutes between the engine's saves while a profile learns.
pub const SAVE_EVERY_MIN: u64 = 5;
const EXTENSION: &str = "bin";
/// A gap between two counts longer than this (standby, a hung tick) is not
/// counted.
const COUNT_GAP: Duration = Duration::from_secs(30);

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct State {
    /// Held by the proxy, for its code benchmark.
    #[serde(default)]
    held_by_proxy: bool,
    /// Held by this service's own benchmark job.
    #[serde(default)]
    held_by_job: bool,
    /// By the model id in lower case.
    #[serde(default)]
    models: BTreeMap<String, ModelState>,
}

impl State {
    fn held(&self) -> bool {
        self.held_by_proxy || self.held_by_job
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct ModelState {
    /// The profile that learns.
    #[serde(default)]
    learning: Option<String>,
    /// The file setup's `--expert-profile` named, as last seen.
    #[serde(default)]
    shipped: Option<String>,
    /// Learned in this round, in milliseconds.
    #[serde(default)]
    learned_ms: u64,
}

/// The service's view of learned expert profiles; the agent keeps one.
pub struct Expert {
    path: PathBuf,
    state: Mutex<State>,
    /// Since when the learning profile counts, as of the last tick.
    counting: Mutex<Option<Instant>>,
}

impl Expert {
    pub fn load(state_dir: &Path) -> Self {
        let path = state_dir.join(STATE_FILE);
        let mut state: State = std::fs::read_to_string(&path)
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        // A job that held learning ended with the service.
        state.held_by_job = false;
        Self {
            path,
            state: Mutex::new(state),
            counting: Mutex::new(None),
        }
    }

    fn keep(&self, state: &State) {
        let text = serde_json::to_string_pretty(state).expect("state serializes");
        if let Err(e) = crate::write_replacing(&self.path, &text) {
            tracing::warn!("{} not written: {e}", self.path.display());
        }
    }

    fn change<T>(&self, change: impl FnOnce(&mut State) -> T) -> T {
        let mut state = self.state.lock().unwrap();
        let out = change(&mut state);
        self.keep(&state);
        out
    }

    /// The profile of model `id` that learns.
    pub fn learning(&self, id: &str) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .models
            .get(&id.to_lowercase())
            .and_then(|m| m.learning.clone())
    }

    /// Some model has a learning profile, held or not: a stop of Strata is
    /// a clean one then.
    pub fn any_learning(&self) -> bool {
        self.state
            .lock()
            .unwrap()
            .models
            .values()
            .any(|m| m.learning.is_some())
    }

    pub fn held(&self) -> bool {
        self.state.lock().unwrap().held()
    }

    pub fn held_by_proxy(&self) -> bool {
        self.state.lock().unwrap().held_by_proxy
    }

    pub fn set_held_by_proxy(&self, on: bool) {
        self.change(|s| s.held_by_proxy = on);
    }

    pub fn held_by_job(&self) -> bool {
        self.state.lock().unwrap().held_by_job
    }

    pub fn set_held_by_job(&self, on: bool) {
        self.change(|s| s.held_by_job = on);
    }

    /// Starts a round of learning `profile` for model `id`, or ends
    /// learning with `None`.
    pub fn set_learning(&self, id: &str, profile: Option<String>) {
        *self.counting.lock().unwrap() = None;
        self.change(|s| {
            let m = s.models.entry(id.to_lowercase()).or_default();
            m.learning = profile;
            m.learned_ms = 0;
        });
    }

    /// Forgets model `id`.
    pub fn forget(&self, id: &str) {
        self.change(|s| s.models.remove(&id.to_lowercase()));
    }

    fn shipped(&self, id: &str) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .models
            .get(&id.to_lowercase())
            .and_then(|m| m.shipped.clone())
    }

    fn note_shipped(&self, id: &str, path: &str) {
        if self.shipped(id).as_deref() == Some(path) {
            return;
        }
        self.change(|s| {
            s.models.entry(id.to_lowercase()).or_default().shipped = Some(path.to_string())
        });
    }

    /// The learning profile of the active model `active`, as `/status`
    /// reports it.
    pub fn status(&self, active: Option<&str>) -> Option<ExpertLearning> {
        let active = active?;
        let state = self.state.lock().unwrap();
        let m = state.models.get(&active.to_lowercase())?;
        Some(ExpertLearning {
            model: active.to_string(),
            profile: m.learning.clone()?,
            held: state.held(),
            secs: m.learned_ms / 1000,
        })
    }

    /// One count: while `counts` holds (Strata answers with the learning
    /// profile of the active model `active`, nothing holds it and no job
    /// runs), the time since the last count is added.
    pub fn tick(&self, active: Option<&str>, counts: bool) {
        let now = Instant::now();
        let mut counting = self.counting.lock().unwrap();
        let Some(active) = active.filter(|_| counts) else {
            *counting = None;
            return;
        };
        let since = counting.replace(now);
        let mut state = self.state.lock().unwrap();
        if state.held() {
            *counting = None;
            return;
        }
        let Some(m) = state
            .models
            .get_mut(&active.to_lowercase())
            .filter(|m| m.learning.is_some())
        else {
            *counting = None;
            return;
        };
        if let Some(since) = since {
            let gap = now.duration_since(since);
            if gap <= COUNT_GAP {
                m.learned_ms += gap.as_millis() as u64;
                // At every watchdog tick: a small file, and a crash loses
                // no more than one tick.
                self.keep(&state);
            }
        }
    }
}

/// The folder of model `id`'s learned profiles.
pub fn folder(cfg: &crate::Config, id: &str) -> PathBuf {
    cfg.strata_home.join(FOLDER).join(id)
}

/// The file of model `id`'s profile `name`.
pub fn file(cfg: &crate::Config, id: &str, name: &str) -> PathBuf {
    folder(cfg, id).join(format!("{name}.{EXTENSION}"))
}

/// What a run config says about its expert profile.
#[derive(Debug, PartialEq, Eq)]
pub enum Seen {
    /// No `--expert-profile` in its args: no profiles for it.
    None,
    /// An expert profile or its saving set by hand.
    ByHand,
    /// Starts with the shipped file at this path.
    Shipped(String),
    /// Starts with this learned profile.
    Learned(String),
}

/// What model `id`'s run config says about its expert profile.
pub fn seen(cfg: &crate::Config, id: &str, model: &Model) -> Seen {
    let Some(arg) = model.expert_profile_arg() else {
        return Seen::None;
    };
    let ours = folder(cfg, id);
    let (args, user) = model.option_names();
    if args.iter().any(|f| f == EXPERT_SAVE_FLAG)
        || user
            .iter()
            .any(|f| f == EXPERT_SAVE_FLAG || f == EXPERT_PROFILE_FLAG)
    {
        return Seen::ByHand;
    }
    match model.expert_save() {
        Some(save) if !in_folder(&save, &ours) => return Seen::ByHand,
        None if model.has_expert_every() => return Seen::ByHand,
        _ => {}
    }
    let path = Path::new(&arg);
    if in_folder(&arg, &ours) {
        return match path.file_stem().and_then(|s| s.to_str()) {
            Some(name) => Seen::Learned(name.to_string()),
            None => Seen::ByHand,
        };
    }
    let in_data = path
        .parent()
        .and_then(Path::file_name)
        .is_some_and(|d| d.eq_ignore_ascii_case("data"));
    if in_data {
        Seen::Shipped(arg)
    } else {
        Seen::ByHand
    }
}

fn in_folder(path: &str, folder: &Path) -> bool {
    Path::new(path)
        .parent()
        .is_some_and(|p| same_path(p, folder))
}

fn same_path(a: &Path, b: &Path) -> bool {
    let norm = |p: &Path| p.to_string_lossy().replace('\\', "/").to_lowercase();
    norm(a).trim_end_matches('/') == norm(b).trim_end_matches('/')
}

/// The learned profiles of model `id` on disk, by name.
fn on_disk(cfg: &crate::Config, id: &str) -> Vec<ExpertProfile> {
    let Ok(entries) = std::fs::read_dir(folder(cfg, id)) else {
        return Vec::new();
    };
    let mut profiles: Vec<ExpertProfile> = entries
        .flatten()
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|x| x.eq_ignore_ascii_case(EXTENSION))
        })
        .filter_map(|e| {
            let name = e.path().file_stem()?.to_str()?.to_string();
            let meta = e.metadata().ok()?;
            let saved = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            Some(ExpertProfile {
                name,
                bytes: Some(meta.len()),
                saved,
            })
        })
        .collect();
    profiles.sort_by(|a, b| a.name.cmp(&b.name));
    profiles
}

/// Model `id`'s expert profiles as `GET /strata` reports them; `None` for
/// a Strata folder from source and a run config without `--expert-profile`.
pub fn profiles(
    agent: &crate::Agent,
    install: &Installation,
    id: &str,
    model: &Model,
) -> Option<ExpertProfiles> {
    if install.kind() != Kind::Terminal {
        return None;
    }
    let cfg = &agent.cfg;
    let expert = &agent.expert;
    let learning = expert.learning(id);
    let mut profiles = on_disk(cfg, id);
    let (active, by_hand) = match seen(cfg, id, model) {
        Seen::None => return None,
        Seen::ByHand => (None, true),
        Seen::Shipped(path) => {
            expert.note_shipped(id, &path);
            (learning.clone(), false)
        }
        Seen::Learned(name) => (learning.clone().or(Some(name)), false),
    };
    if let Some(name) = &learning
        && !by_hand
        && !profiles.iter().any(|p| &p.name == name)
    {
        profiles.push(ExpertProfile {
            name: name.clone(),
            bytes: None,
            saved: None,
        });
        profiles.sort_by(|a, b| a.name.cmp(&b.name));
    }
    Some(ExpertProfiles {
        learning: !by_hand && learning.is_some(),
        held: !by_hand && learning.is_some() && expert.held(),
        active: if by_hand { None } else { active },
        by_hand,
        profiles,
    })
}

/// The shipped file for model `id`: setup's file name in the `data` folder
/// of the run config's `cwd`, so a newer Control folder gives the right
/// path; without a `cwd` the path as last seen.
fn shipped_path(agent: &crate::Agent, id: &str, model: &Model) -> Option<String> {
    let seen = match seen(&agent.cfg, id, model) {
        Seen::Shipped(path) => Some(path),
        _ => agent.expert.shipped(id),
    }?;
    let name = Path::new(&seen).file_name()?.to_os_string();
    Some(match model.cwd() {
        Some(cwd) => Path::new(&cwd)
            .join("data")
            .join(name)
            .display()
            .to_string(),
        None => seen,
    })
}

/// Why a profile job for model `id` cannot run, if it cannot.
pub fn refused(agent: &crate::Agent, install: &Installation, id: &str) -> Option<String> {
    if install.kind() != Kind::Terminal {
        return Some(
            "Expert profiles need the Strata Terminal; this Strata is a folder from source".into(),
        );
    }
    let model = install.read_model(id);
    match seen(&agent.cfg, id, &model) {
        Seen::None => Some(format!(
            "The run config of {id} names no expert profile, so it has none to learn"
        )),
        Seen::ByHand => Some("Expert profile set by hand in the run config".into()),
        Seen::Shipped(path) => {
            agent.expert.note_shipped(id, &path);
            None
        }
        Seen::Learned(_) => None,
    }
}

/// Why `name` cannot be a profile to use or learn, if it cannot.
pub fn bad_name(name: &str) -> Option<String> {
    profile_name_problem(name)
}

/// Whether model `id` has a saved file for profile `name`.
pub fn saved(cfg: &crate::Config, id: &str, name: &str) -> bool {
    file(cfg, id, name).is_file()
}

/// What model `id`'s run config gets for `active` (`None`: the shipped
/// profile) learning into `learning`: the file `--expert-profile` names and
/// the file the engine saves into, none while learning is held.
pub fn planned(
    agent: &crate::Agent,
    id: &str,
    model: &Model,
    active: Option<&str>,
    learning: Option<&str>,
) -> Result<(String, Option<String>), String> {
    let cfg = &agent.cfg;
    let shipped = shipped_path(agent, id, model)
        .ok_or_else(|| format!("The shipped expert profile of {id} is not known"))?;
    let start = match active {
        Some(name) if saved(cfg, id, name) => file(cfg, id, name).display().to_string(),
        // A profile that never saved starts from the shipped one.
        _ => shipped,
    };
    let save = learning
        .filter(|_| !agent.expert.held())
        .map(|name| file(cfg, id, name).display().to_string());
    if save.is_some() {
        std::fs::create_dir_all(folder(cfg, id))
            .map_err(|e| format!("{} not made: {e}", folder(cfg, id).display()))?;
    }
    Ok((start, save))
}

/// Puts what [`planned`] gave into a run config.
pub fn apply(model: &mut Model, start: &str, save: Option<&str>) {
    model.set_expert_profile_arg(start);
    model.set_expert_save(save.map(|s| (s, SAVE_EVERY_MIN)));
}

/// Brings every run config's save keys to what the state says: the
/// learning profile's unless learning is held, none otherwise. Leaves run
/// configs set by hand alone. Gives the ids whose config changed.
pub fn manage(agent: &crate::Agent, install: &Installation) -> Vec<String> {
    if install.kind() != Kind::Terminal {
        return Vec::new();
    }
    let cfg = &agent.cfg;
    let held = agent.expert.held();
    install
        .models()
        .into_iter()
        .filter_map(|id| {
            let id = crate::strata::model_id(agent, install, &id);
            let mut model = install.model(&id).ok()?;
            match seen(cfg, &id, &model) {
                Seen::None | Seen::ByHand => return None,
                Seen::Shipped(path) => agent.expert.note_shipped(&id, &path),
                Seen::Learned(_) => {}
            }
            let wanted = agent
                .expert
                .learning(&id)
                .filter(|_| !held)
                .map(|name| file(cfg, &id, &name).display().to_string());
            let now = model.expert_save();
            if now == wanted && (wanted.is_none() || model.has_expert_every()) {
                return None;
            }
            model.set_expert_save(wanted.as_deref().map(|s| (s, SAVE_EVERY_MIN)));
            if wanted.is_some()
                && let Err(e) = std::fs::create_dir_all(folder(cfg, &id))
            {
                tracing::warn!("{} not made: {e}", folder(cfg, &id).display());
            }
            match install.save(&model) {
                Ok(()) => Some(id),
                Err(e) => {
                    tracing::warn!("Expert profile keys not written: {e}");
                    None
                }
            }
        })
        .collect()
}

/// Deletes profile `name` of model `id`.
pub fn delete_file(cfg: &crate::Config, id: &str, name: &str) -> Result<(), String> {
    let path = file(cfg, id, name);
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("{} not deleted: {e}", path.display())),
    }
}

/// Deletes every learned profile of model `id`.
pub fn delete_all(cfg: &crate::Config, id: &str) {
    let dir = folder(cfg, id);
    if let Err(e) = std::fs::remove_dir_all(&dir)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        tracing::warn!("{} not deleted: {e}", dir.display());
    }
}

/// `STANDARD_PROFILE` is no name a learned profile takes.
pub fn is_standard(name: &str) -> bool {
    name.eq_ignore_ascii_case(STANDARD_PROFILE)
}
