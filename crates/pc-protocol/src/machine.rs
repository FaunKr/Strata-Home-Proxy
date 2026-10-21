//! The machine facts in `GET /strata`: what the PC is, read once by the
//! PC service. Not the hardware readings, which change all the time.

use serde::{Deserialize, Serialize};

/// What the PC is. Every value the PC service could not read is `None`
/// (`null`) or an empty list.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct MachineFacts {
    /// One entry per filled RAM slot.
    #[serde(default)]
    pub ram_sticks: Vec<RamStick>,
    /// RAM slots on the board, filled or not.
    #[serde(default)]
    pub ram_slots: Option<u32>,
    /// Every graphics card with VRAM of its own, the most VRAM first.
    #[serde(default)]
    pub gpus: Vec<GpuFacts>,
    #[serde(default)]
    pub cpu: CpuFacts,
    #[serde(default)]
    pub windows: WindowsFacts,
    /// The drive that holds the Strata data folder: `nvme`, `ssd` (another
    /// SSD, e.g. SATA) or `hdd`. A string, so a later kind does not break
    /// older proxies.
    #[serde(default)]
    pub model_drive: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RamStick {
    #[serde(default)]
    pub size_bytes: Option<u64>,
    /// E.g. `DDR5`.
    #[serde(default)]
    pub kind: Option<String>,
    /// The speed it is rated for, in MT/s.
    #[serde(default)]
    pub rated_mts: Option<u32>,
    /// The speed it runs at, in MT/s.
    #[serde(default)]
    pub configured_mts: Option<u32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct GpuFacts {
    #[serde(default)]
    pub name: Option<String>,
    /// Dedicated VRAM.
    #[serde(default)]
    pub vram_bytes: Option<u64>,
    /// As Windows shows it, e.g. `32.0.15.6094`.
    #[serde(default)]
    pub driver_version: Option<String>,
    /// The PCIe generation and lanes the card can do; a slower slot shows
    /// only in the link now.
    #[serde(default)]
    pub pcie_max_gen: Option<u8>,
    #[serde(default)]
    pub pcie_max_lanes: Option<u8>,
    /// The link now; an idle card often runs a lower generation.
    #[serde(default)]
    pub pcie_gen: Option<u8>,
    #[serde(default)]
    pub pcie_lanes: Option<u8>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct CpuFacts {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub cores: Option<u32>,
    #[serde(default)]
    pub threads: Option<u32>,
    /// The base clock in MHz, as Windows reports it; not the boost clock.
    #[serde(default)]
    pub base_mhz: Option<u32>,
    #[serde(default)]
    pub avx2: Option<bool>,
    #[serde(default)]
    pub avx512: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct WindowsFacts {
    /// E.g. `Windows 11 Pro`.
    #[serde(default)]
    pub edition: Option<String>,
    /// E.g. `24H2`.
    #[serde(default)]
    pub version: Option<String>,
    /// E.g. `26100.4652`.
    #[serde(default)]
    pub build: Option<String>,
}
