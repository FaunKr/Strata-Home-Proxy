//! Paths of a run config into a Strata folder an update deleted. Setup
//! writes absolute paths into the folder it ran from: after a `strata
//! update` that is a Control folder `<components>\control\<version>` (`cwd`,
//! `--expert-profile`, the speed projection's vector), and the vision
//! reader's program lies in an engine folder `<components>\engine\<name>`.
//! Each update keeps the new and the previous folder and deletes older
//! ones, and nothing rewrites the run configs (`strata_control/updater.py`).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Where in Strata's component folder a path lies.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Area {
    Control,
    Engine,
}

impl Area {
    fn folder(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Engine => "engine",
        }
    }
}

/// The folders stale paths move to: the Control folder of the Strata files
/// that run, and the engine folder Strata picks for this PC, if known.
pub struct Targets<'a> {
    pub control: &'a Path,
    pub engine: Option<&'a Path>,
}

/// What a move did to one run config.
#[derive(Debug, Default, PartialEq)]
pub struct Moved {
    /// Paths moved, by the deleted folder's name and the folder they moved to.
    pub moved: BTreeMap<(String, PathBuf), usize>,
    /// Stale paths that stayed: no folder to move to, or no such file there.
    pub left: Vec<String>,
}

/// The areas of `value`'s stale paths: texts anywhere in it, also as the
/// value of an option `--x=<path>`, below a folder of `components` that is
/// not there.
pub fn stale(value: &Value, components: &Path) -> Vec<Area> {
    let mut areas = Vec::new();
    each_text(value, &mut |text| {
        if let Some(path) = stale_path(text, components) {
            areas.push(path.area);
        }
    });
    areas.sort();
    areas.dedup();
    areas
}

/// Moves `value`'s stale paths to `targets`: each only when the same file
/// is there below the new folder.
pub fn move_stale(value: &mut Value, components: &Path, targets: &Targets) -> Moved {
    let mut moved = Moved::default();
    each_text_mut(value, &mut |text| {
        let Some(path) = stale_path(text, components) else {
            return;
        };
        let target = match path.area {
            Area::Control => Some(targets.control),
            Area::Engine => targets.engine,
        };
        let new = target.and_then(|t| moved_path(t, &path.rest));
        match (target, new) {
            (Some(target), Some(new)) => {
                *moved
                    .moved
                    .entry((path.name.clone(), target.to_path_buf()))
                    .or_default() += 1;
                *text = format!("{}{}", path.option, new.display());
            }
            _ => moved.left.push(text.clone()),
        }
    });
    moved
}

/// `rest` below `target`, if that file is there. A last part may carry a
/// value after a colon, as the speed projection's vector does
/// (`<file>:1.0`, `--control-vector-scaled`): then the file before it counts.
fn moved_path(target: &Path, rest: &[String]) -> Option<PathBuf> {
    let new = rest
        .iter()
        .fold(target.to_path_buf(), |p, part| p.join(part));
    if new.exists() {
        return Some(new);
    }
    let (last, dirs) = rest.split_last()?;
    let (file, _) = last.rsplit_once(':')?;
    let folder = dirs
        .iter()
        .fold(target.to_path_buf(), |p, part| p.join(part));
    folder.join(file).is_file().then(|| folder.join(last))
}

/// A path below `<components>\<area>\<name>\` whose folder `<name>` is gone.
struct StalePath {
    area: Area,
    name: String,
    /// The parts below `<name>`.
    rest: Vec<String>,
    /// `--x=` when the path is an option's value, else empty.
    option: String,
}

fn stale_path(text: &str, components: &Path) -> Option<StalePath> {
    let (option, path) = match text.split_once('=') {
        Some((option, path)) if option.starts_with('-') => (format!("{option}="), path),
        _ => (String::new(), text),
    };
    let components_text = components.to_string_lossy();
    let base = parts(&components_text);
    let all = parts(path);
    if base.is_empty() || all.len() < base.len() + 2 {
        return None;
    }
    let same = |a: &str, b: &str| a.eq_ignore_ascii_case(b);
    if !all.iter().zip(&base).all(|(a, b)| same(a, b)) {
        return None;
    }
    let area = [Area::Control, Area::Engine]
        .into_iter()
        .find(|area| same(all[base.len()], area.folder()))?;
    let name = all[base.len() + 1].to_string();
    if components.join(area.folder()).join(&name).is_dir() {
        return None;
    }
    Some(StalePath {
        area,
        name,
        rest: all[base.len() + 2..]
            .iter()
            .map(|p| p.to_string())
            .collect(),
        option,
    })
}

/// A path's parts, with `\` and `/` alike; a drive (`C:`) is a part.
fn parts(path: &str) -> Vec<&str> {
    path.split(['\\', '/']).filter(|p| !p.is_empty()).collect()
}

fn each_text(value: &Value, f: &mut impl FnMut(&str)) {
    match value {
        Value::String(text) => f(text),
        Value::Array(items) => items.iter().for_each(|v| each_text(v, f)),
        Value::Object(fields) => fields.values().for_each(|v| each_text(v, f)),
        _ => {}
    }
}

fn each_text_mut(value: &mut Value, f: &mut impl FnMut(&mut String)) {
    match value {
        Value::String(text) => f(text),
        Value::Array(items) => items.iter_mut().for_each(|v| each_text_mut(v, f)),
        Value::Object(fields) => fields.values_mut().for_each(|v| each_text_mut(v, f)),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A component folder with Control 0.1.52 and one engine folder; the
    /// Control 0.1.49.1 and engine 0.1.48 that run configs name are gone.
    struct Folders {
        root: PathBuf,
    }

    impl Folders {
        fn new(name: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("stale-paths-{name}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&root);
            for file in [
                "control/0.1.52/data/expert-profile.bin",
                "engine/0.1.50-windows-x86_64-hip-rdna2/strata-vision.exe",
            ] {
                let path = root.join("components").join(file);
                std::fs::create_dir_all(path.parent().unwrap()).unwrap();
                std::fs::write(path, "x").unwrap();
            }
            Self { root }
        }

        fn components(&self) -> PathBuf {
            self.root.join("components")
        }

        fn old(&self, rest: &str) -> String {
            format!("{}/{rest}", self.components().display())
        }
    }

    impl Drop for Folders {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn paths_into_deleted_folders_move_with_the_rest_of_the_path() {
        let f = Folders::new("move");
        let components = f.components();
        let control = components.join("control").join("0.1.52");
        let engine = components
            .join("engine")
            .join("0.1.50-windows-x86_64-hip-rdna2");
        let mut config = json!({
            "cwd": f.old("control/0.1.49.1"),
            "args": ["--expert-profile", f.old("control/0.1.49.1/data/expert-profile.bin"),
                "--control-vector-scaled", f.old("control/0.1.49.1/data/expert-profile.bin:1.0")],
            "user_args": [format!("--x={}", f.old("CONTROL/0.1.49.1/data/expert-profile.bin"))],
            "vision": {"exe": f.old("engine/0.1.48-windows-x86_64-hip-rdna2/strata-vision.exe"), "gpu": false},
        });
        assert_eq!(
            stale(&config, &components),
            vec![Area::Control, Area::Engine]
        );
        let moved = move_stale(
            &mut config,
            &components,
            &Targets {
                control: &control,
                engine: Some(&engine),
            },
        );
        assert_eq!(config["cwd"], control.display().to_string());
        assert_eq!(
            config["args"][1],
            control
                .join("data")
                .join("expert-profile.bin")
                .display()
                .to_string()
        );
        assert_eq!(
            config["args"][3],
            format!(
                "{}:1.0",
                control.join("data").join("expert-profile.bin").display()
            )
        );
        assert_eq!(
            config["user_args"][0],
            format!(
                "--x={}",
                control.join("data").join("expert-profile.bin").display()
            )
        );
        assert_eq!(
            config["vision"]["exe"],
            engine.join("strata-vision.exe").display().to_string()
        );
        assert_eq!(moved.moved[&("0.1.49.1".to_string(), control.clone())], 4);
        assert_eq!(
            moved.moved[&("0.1.48-windows-x86_64-hip-rdna2".to_string(), engine)],
            1
        );
        assert!(moved.left.is_empty());
        assert!(stale(&config, &components).is_empty());
    }

    #[test]
    fn a_path_stays_when_its_folder_is_there_or_the_new_one_lacks_its_file() {
        let f = Folders::new("stay");
        let components = f.components();
        let control = components.join("control").join("0.1.52");
        let there = f.old("control/0.1.52/data/expert-profile.bin");
        let missing = f.old("control/0.1.49.1/data/learned.bin");
        let engine = f.old("engine/0.1.48-windows-x86_64-hip-rdna2/strata-vision.exe");
        let mut config =
            json!({"args": [there, missing, "--native", "C:/models/a.gguf"], "exe": engine});
        let before = config.clone();
        let moved = move_stale(
            &mut config,
            &components,
            &Targets {
                control: &control,
                engine: None,
            },
        );
        assert_eq!(config, before);
        assert_eq!(moved.left, vec![missing, engine]);
        assert!(moved.moved.is_empty());
    }

    #[test]
    fn paths_outside_the_component_folder_are_never_stale() {
        let f = Folders::new("outside");
        let config = json!({
            "tokenizer": "C:\\Strata-data\\packs\\q2_0\\tokenizer",
            "args": ["--kv", "int8", "--expert-profile-save", "expert-profile-learned.bin"],
            "cwd": f.old("control"),
            "port": 8080,
        });
        assert!(stale(&config, &f.components()).is_empty());
    }
}
