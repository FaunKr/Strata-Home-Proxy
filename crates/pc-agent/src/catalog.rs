//! The models Strata's setup offers, from its tables (`MODELS`, `FAMILIES`,
//! `CONTEXTS` in `setup.py`, Strata commit 477ac13). Only values from here
//! reach the setup; there are no free arguments. Left out: the NVIDIA-only
//! NVFP4 packs.

use serde::Serialize;

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Family {
    pub id: &'static str,
    pub title: &'static str,
    /// Prefix of the model id: setup names config, start script and model
    /// folder after tag + size.
    #[serde(skip)]
    pub tag: &'static str,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Size {
    pub family: &'static str,
    pub size: &'static str,
    pub download_gb: f64,
    pub ram_gb: u32,
}

pub const FAMILIES: &[Family] = &[
    Family {
        id: "qwen",
        title: "Qwen3.8-Flash-Next",
        tag: "",
    },
    Family {
        id: "swift",
        title: "Swift 1.5",
        tag: "swift-",
    },
    Family {
        id: "coder",
        title: "Qwen3.8-Flash-Next Coder",
        tag: "coder-",
    },
    Family {
        id: "unsloth",
        title: "Qwen3.8-Flash-Next (Unsloth)",
        tag: "unsloth-",
    },
    Family {
        id: "qwen36",
        title: "Qwen3.6-35B-A3B",
        tag: "qwen36-",
    },
];

const fn size(family: &'static str, size: &'static str, download_gb: f64, ram_gb: u32) -> Size {
    Size {
        family,
        size,
        download_gb,
        ram_gb,
    }
}

pub const SIZES: &[Size] = &[
    size("qwen", "Q2_0", 66.4, 48),
    size("qwen", "IQ2_XS", 68.0, 48),
    size("qwen", "IQ3_XXS", 75.8, 60),
    size("qwen", "IQ3_S", 83.6, 62),
    size("swift", "IQ2_XS", 68.0, 48),
    size("swift", "IQ3_XXS", 75.8, 60),
    size("swift", "IQ3_S", 83.6, 62),
    size("coder", "IQ1_M", 58.4, 32),
    size("unsloth", "UD-IQ4_XS", 93.7, 48),
    size("unsloth", "UD-Q4_K_XL", 111.3, 48),
    size("unsloth", "UD-Q5_K_XL", 158.3, 108),
    size("unsloth", "UD-Q6_K_XL", 169.2, 118),
    size("unsloth", "Q8_0", 188.2, 136),
    size("qwen36", "UD-Q4_K_XL", 22.9, 32),
    size("qwen36", "UD-Q6_K_XL", 32.6, 44),
    size("qwen36", "Q8_0", 37.8, 52),
];

pub const CONTEXTS: &[u32] = &[8192, 32768, 65536, 131072, 204800, 262144, 393216, 524288];

/// Setup's pick for a 16 GB card such as the RX 6800 XT (32K under 14 GB of
/// VRAM, 64K under 20 GB, else 128K).
pub const DEFAULT_CONTEXT: u32 = 65536;

/// Free space the setup needs beside the download.
pub const DISK_SPARE_GB: f64 = 8.0;

pub fn family(id: &str) -> Option<&'static Family> {
    FAMILIES.iter().find(|f| f.id == id)
}

pub fn find(family: &str, size: &str) -> Option<&'static Size> {
    SIZES.iter().find(|s| s.family == family && s.size == size)
}

impl Size {
    /// The model id setup uses: `IQ3_XXS`, `coder-IQ1_M`.
    pub fn id(&self) -> String {
        let tag = family(self.family).map_or("", |f| f.tag);
        format!("{tag}{}", self.size)
    }
}

/// The size an id names, in any letter case (`strata-coder-iq1_m.json`).
pub fn by_id(id: &str) -> Option<&'static Size> {
    SIZES.iter().find(|s| s.id().eq_ignore_ascii_case(id))
}

/// Setup's own pick for a PC with `ram_gb` of RAM.
pub fn recommend(ram_gb: f64) -> Option<&'static Size> {
    if ram_gb >= 60.0 {
        find("qwen", "IQ3_XXS")
    } else if ram_gb >= 44.0 {
        find("qwen", "Q2_0")
    } else if ram_gb >= 28.0 {
        find("coder", "IQ1_M")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_follow_setup() {
        assert_eq!(find("qwen", "IQ3_XXS").unwrap().id(), "IQ3_XXS");
        assert_eq!(find("coder", "IQ1_M").unwrap().id(), "coder-IQ1_M");
        assert_eq!(by_id("coder-iq1_m").unwrap().family, "coder");
    }

    #[test]
    fn every_size_has_a_family() {
        assert!(SIZES.iter().all(|s| family(s.family).is_some()));
    }
}
