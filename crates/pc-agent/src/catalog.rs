//! Strata's model catalog as the installed Strata reads it itself: through
//! `tools/strata_mcp.py` (`Strata.tables`, `Strata.model_table`,
//! `Strata.recommend` with Strata's own hardware detection), the one
//! catalog Strata's desktop app and `strata launch` use. The PC service
//! keeps no table of its own; only values from the catalog read last reach
//! setup.

use serde_json::Value;

/// Prints Strata's catalog for this PC as one JSON line: `source` (where
/// Strata took its tables from), `contexts`, `catalog` (`model_table`) and
/// `recommend`, or `error`. Runs in the folder that holds `setup.py` and
/// `tools/`; arguments: the folder with the run configs and the data folder,
/// whose free space Strata's verdict counts. Its first line names it for the
/// tests' fake commands.
pub const SCRIPT: &str = r#"# catalog
import importlib.util, json, sys
from pathlib import Path
out = {}
try:
    src = Path.cwd()
    spec = importlib.util.spec_from_file_location("strata_mcp_catalog", src / "tools" / "strata_mcp.py")
    mcp = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mcp)
    s = mcp.Strata(sys.argv[1], src)
    _models, _families, contexts, source = s.tables()
    hw = s.hardware()
    hw["disk_free_gb"] = s.disk_free(Path(sys.argv[2]))
    out = {"source": source, "contexts": contexts, "catalog": s.model_table(hw)}
    try:
        out["recommend"] = s.recommend(hw)
    except Exception as e:
        out["recommend_error"] = f"{type(e).__name__}: {e}"
except BaseException as e:
    out = {"error": f"{type(e).__name__}: {e}"}
print(json.dumps(out))
"#;

/// Where Strata's tables come from when it read them; strata_mcp's own
/// fallback table (`setup.py` not importable) does not count.
const FROM_SETUP: &str = "setup.py";

/// The context when Strata recommends none (no card it can use).
pub const DEFAULT_CONTEXT: u32 = 65536;

/// Free space the setup needs beside the download.
pub const DISK_SPARE_GB: f64 = 8.0;

/// The picture file setup downloads with `--vision yes`: 0.91 GB for the
/// Qwen3.8-Flash-Next families, 0.90 GB for Qwen3.6 (`sha256` sizes of
/// `mmproj-Qwen3.8-Flash-Next-BF16.gguf` and `mmproj-F16.gguf` in `setup.py`).
pub const PICTURES_GB: f64 = 0.91;

/// What Strata offers on this PC.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Catalog {
    pub families: Vec<Family>,
    pub sizes: Vec<Size>,
    pub contexts: Vec<u32>,
    /// The context Strata picks for this PC's card.
    pub default_context: u32,
    /// The id of the size Strata recommends for this PC, if any.
    pub recommended: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Family {
    pub id: String,
    pub title: String,
    /// Some size of the family reads pictures.
    pub pictures: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Size {
    /// The model id setup names the run config and the download after,
    /// in Strata's letter case: `IQ3_XXS`, `coder-IQ1_M`.
    pub id: String,
    pub family: String,
    pub size: String,
    /// Strata's one line about it.
    pub about: String,
    pub download_gb: f64,
    /// The memory it needs, as Strata states it.
    pub ram_gb: f64,
    /// `ram_gb` counts RAM and VRAM together (Gyro).
    pub with_vram: bool,
    /// Strata's verdict for this PC, e.g. `fits`, `tight: …`.
    pub verdict: Option<String>,
    /// The verdict says it fits (also in the low-RAM mode or partly from
    /// the SSD), as `strata launch` sorts the verdicts.
    pub fits: bool,
    pub experimental: bool,
    pub pictures: bool,
}

impl Catalog {
    pub fn find(&self, family: &str, size: &str) -> Option<&Size> {
        self.sizes
            .iter()
            .find(|s| s.family == family && s.size == size)
    }

    /// The size an id names, in any letter case (`strata-coder-iq1_m.json`).
    pub fn by_id(&self, id: &str) -> Option<&Size> {
        self.sizes.iter().find(|s| s.id.eq_ignore_ascii_case(id))
    }
}

/// The catalog from what [`SCRIPT`] printed, or why there is none.
pub fn read(output: &[String]) -> Result<Catalog, String> {
    let answer = output
        .iter()
        .rev()
        .find_map(|line| {
            serde_json::from_str::<Value>(line.trim())
                .ok()
                .filter(Value::is_object)
        })
        .ok_or("Strata gave no catalog")?;
    if let Some(error) = answer["error"].as_str() {
        return Err(format!("Strata's catalog could not be read: {error}"));
    }
    if answer["source"] != FROM_SETUP {
        return Err("Strata could not read setup's tables".into());
    }
    let mut families = Vec::new();
    let mut sizes = Vec::new();
    for family in answer["catalog"].as_array().into_iter().flatten() {
        let Some(id) = family["family"].as_str() else {
            continue;
        };
        let before = sizes.len();
        sizes.extend(
            family["sizes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|s| size(id, s)),
        );
        families.push(Family {
            id: id.to_string(),
            title: family["title"].as_str().unwrap_or(id).to_string(),
            pictures: sizes[before..].iter().any(|s| s.pictures),
        });
    }
    if sizes.is_empty() {
        return Err("Strata's catalog has no models".into());
    }
    let contexts: Vec<u32> = answer["contexts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c.as_u64().and_then(|c| u32::try_from(c).ok()))
        .collect();
    let recommend = &answer["recommend"];
    let recommended = match (recommend["family"].as_str(), recommend["model"].as_str()) {
        (Some(family), Some(model)) => sizes
            .iter()
            .find(|s| s.family == family && s.size == model)
            .map(|s| s.id.clone()),
        _ => None,
    };
    let default_context = recommend["context"]
        .as_u64()
        .and_then(|c| u32::try_from(c).ok())
        .filter(|c| contexts.contains(c))
        .unwrap_or(DEFAULT_CONTEXT);
    Ok(Catalog {
        families,
        sizes,
        contexts,
        default_context,
        recommended,
    })
}

/// A size of `model_table`. Its `id` is the run config's lower-case tag;
/// setup names the download after the tag's prefix and the size as
/// written (`coder-IQ1_M`).
fn size(family: &str, s: &Value) -> Option<Size> {
    let size = s["model"].as_str()?;
    let tag = s["id"].as_str()?;
    let prefix = tag
        .len()
        .checked_sub(size.len())
        .filter(|&at| tag.is_char_boundary(at) && tag[at..].eq_ignore_ascii_case(size))
        .map(|at| &tag[..at])?;
    let verdict = s["on_this_pc"].as_str().map(String::from);
    Some(Size {
        id: format!("{prefix}{size}"),
        family: family.to_string(),
        size: size.to_string(),
        about: s["about"].as_str().unwrap_or_default().to_string(),
        download_gb: s["download_gb"].as_f64()?,
        ram_gb: s["ram_needed_gb"].as_f64()?,
        with_vram: s["mem_total"].as_bool().unwrap_or(false),
        fits: verdict.as_deref().is_some_and(fits),
        verdict,
        experimental: s["experimental"].as_bool().unwrap_or(false),
        pictures: s["images"].as_bool().unwrap_or(false),
    })
}

/// Whether Strata's verdict says the size fits, as `verdict` in
/// `strata_cli/launch/catalog.py` sorts them: also in the low-RAM mode or
/// partly read from the SSD, not when it is tight or slow there.
fn fits(verdict: &str) -> bool {
    let low = verdict.trim().to_lowercase();
    if low.starts_with("does not fit") || low.contains("free disk space") {
        return false;
    }
    if low.contains("low-ram mode") {
        return true;
    }
    if low.contains("part of its experts") || low.contains("read from the ssd") {
        return !low.contains("slow here");
    }
    !low.starts_with("tight") && low.contains("fits")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What [`SCRIPT`] printed with Strata at 8183c6b6 for a PC with 64 GB
    /// of RAM and an RTX 4090 (24 GB): `strata_mcp.py`'s own output, its
    /// hardware detection given that PC.
    const RTX_4090: &str = include_str!("../fixtures/strata-catalog.json");

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(String::from).collect()
    }

    #[test]
    fn strata_s_catalog_names_every_size_as_setup_does() {
        let catalog = read(&lines(RTX_4090)).unwrap();
        let ids: Vec<_> = catalog.sizes.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"IQ3_XXS"), "{ids:?}");
        assert!(ids.contains(&"coder-IQ1_M"), "{ids:?}");
        assert!(ids.contains(&"gyro-Gyro-S"), "{ids:?}");
        assert!(ids.contains(&"deepseek-v4.1-flash-UD-Q8_K_XL"), "{ids:?}");
        let coder = catalog.find("coder", "IQ1_M").unwrap();
        assert_eq!(catalog.by_id("CODER-iq1_m"), Some(coder));
        assert_eq!(coder.download_gb, 58.4);
        assert_eq!(coder.ram_gb, 32.0);
        assert!(coder.pictures);
        let gyro = catalog.by_id("gyro-Gyro-S").unwrap();
        assert!(gyro.with_vram);
        assert_eq!(gyro.ram_gb, 27.6);
        let deepseek = catalog
            .families
            .iter()
            .find(|f| f.id == "deepseek41")
            .unwrap();
        assert_eq!(deepseek.title, "DeepSeek-V4.1-Flash");
    }

    #[test]
    fn the_verdicts_say_what_fits() {
        let catalog = read(&lines(RTX_4090)).unwrap();
        let fits = |id: &str| catalog.by_id(id).unwrap().fits;
        assert!(fits("IQ3_S"));
        assert!(fits("unsloth-UD-IQ4_XS"), "part read from the SSD");
        assert!(!fits("unsloth-Q8_0"), "slow here");
        assert!(fits("nvfp4-NVFP4"), "the low-RAM mode");
        assert!(!fits("deepseek-v4.1-flash-IST-DAS-2.00bpw"), "disk space"); // language-check: allow
        assert!(!fits("deepseek-v4.1-flash-UD-Q8_K_XL"), "does not fit");
        let verdict = catalog.by_id("unsloth-Q8_0").unwrap().verdict.as_deref();
        assert!(verdict.unwrap().starts_with("slow here"), "{verdict:?}");
    }

    #[test]
    fn strata_s_recommendation_and_contexts() {
        let catalog = read(&lines(RTX_4090)).unwrap();
        assert_eq!(catalog.recommended.as_deref(), Some("IQ3_S"));
        assert_eq!(catalog.default_context, 131072);
        assert_eq!(
            catalog.contexts,
            [8192, 32768, 65536, 131072, 204800, 262144, 393216, 524288]
        );
    }

    #[test]
    fn without_a_card_strata_recommends_nothing() {
        let mut answer: Value = serde_json::from_str(RTX_4090).unwrap();
        answer["recommend"] = serde_json::json!({"family": null, "model": null,
            "why": "no NVIDIA RTX 20-series-or-newer GPU found (nvidia-smi did not list one)"});
        let catalog = read(&[answer.to_string()]).unwrap();
        assert_eq!(catalog.recommended, None);
        assert_eq!(catalog.default_context, DEFAULT_CONTEXT);
    }

    #[test]
    fn a_catalog_that_strata_did_not_read_from_setup_is_none() {
        let mut answer: Value = serde_json::from_str(RTX_4090).unwrap();
        answer["source"] = "built-in table (setup.py not importable)".into();
        assert!(read(&[answer.to_string()]).is_err());
        let traceback = lines("Traceback (most recent call last):\n  File \"<string>\", line 1");
        assert_eq!(read(&traceback).unwrap_err(), "Strata gave no catalog");
        let failed = read(&lines(
            r#"{"error": "FileNotFoundError: [Errno 2] No such file or directory: 'tools/strata_mcp.py'"}"#,
        ))
        .unwrap_err();
        assert!(failed.contains("strata_mcp.py"), "{failed}");
    }
}
