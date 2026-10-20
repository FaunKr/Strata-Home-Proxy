//! The models Strata's setup offers, from its tables (`MODELS`, `FAMILIES`,
//! `CONTEXTS`, `MEMORY_TIERS` in `setup.py`, Strata commit cbfd5b9). Only values from here
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
    /// Setup offers Strata's experimental speed projection; made for the
    /// original Qwen3.8-Flash-Next, which the Coder shares.
    pub speed_projection: bool,
}

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Size {
    pub family: &'static str,
    pub size: &'static str,
    pub download_gb: f64,
    pub ram_gb: u32,
    /// `ram_gb` counts RAM and VRAM together (setup's `mem_total`: Gyro).
    pub with_vram: bool,
}

pub const FAMILIES: &[Family] = &[
    Family {
        id: "qwen",
        title: "Qwen3.8-Flash-Next",
        tag: "",
        speed_projection: true,
    },
    Family {
        id: "swift",
        title: "Swift 1.5",
        tag: "swift-",
        speed_projection: false,
    },
    Family {
        id: "coder",
        title: "Qwen3.8-Flash-Next Coder",
        tag: "coder-",
        speed_projection: true,
    },
    Family {
        id: "unsloth",
        title: "Qwen3.8-Flash-Next (Unsloth)",
        tag: "unsloth-",
        speed_projection: false,
    },
    Family {
        id: "qwen36",
        title: "Qwen3.6-35B-A3B",
        tag: "qwen36-",
        speed_projection: false,
    },
    Family {
        id: "gyro",
        title: "Qwen3.8-Flash-Next (Gyro)",
        tag: "gyro-",
        speed_projection: false,
    },
];

const fn size(family: &'static str, size: &'static str, download_gb: f64, ram_gb: u32) -> Size {
    Size {
        family,
        size,
        download_gb,
        ram_gb,
        with_vram: false,
    }
}

/// A size whose memory is RAM and VRAM together; setup's 27.6 GB rounded up.
const fn with_vram(family: &'static str, size: &'static str, download_gb: f64, gb: u32) -> Size {
    Size {
        with_vram: true,
        ..self::size(family, size, download_gb, gb)
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
    with_vram("gyro", "Gyro-S", 58.5, 28),
    with_vram("gyro", "Gyro-M", 92.0, 35),
];

/// Setup's `MEMORY_TIERS`: the size for RAM + VRAM from each floor, 2 GB
/// below the tiers' names because the OS reports a little less.
const MEMORY_TIERS: [(f64, &str, &str); 3] = [
    (62.0, "qwen", "IQ3_S"),
    (46.0, "qwen", "IQ3_XXS"),
    (38.0, "swift", "IQ2_XS"),
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

/// The size an id names, in any letter case (`strata-coder-iq1_m.json`).
pub fn by_id(id: &str) -> Option<&'static Size> {
    SIZES.iter().find(|s| s.id().eq_ignore_ascii_case(id))
}

impl Size {
    /// The model id setup uses: `IQ3_XXS`, `coder-IQ1_M`.
    pub fn id(&self) -> String {
        let tag = family(self.family).map_or("", |f| f.tag);
        format!("{tag}{}", self.size)
    }

    /// Whether a PC with this RAM and VRAM (GiB, as setup counts) has the
    /// memory for it.
    pub fn fits(&self, ram_gb: f64, vram_gb: f64) -> bool {
        let have = if self.with_vram {
            ram_gb + vram_gb
        } else {
            ram_gb
        };
        f64::from(self.ram_gb) <= have + 0.5
    }
}

/// Setup's own pick for a PC with this RAM and VRAM (GiB): its memory tiers,
/// below them the Coder from 28 GB of RAM. Never Gyro, as setup.
pub fn recommend(ram_gb: f64, vram_gb: f64) -> Option<&'static Size> {
    if let Some((_, family, size)) = MEMORY_TIERS
        .iter()
        .find(|(floor, _, _)| ram_gb + vram_gb >= *floor)
    {
        find(family, size)
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
