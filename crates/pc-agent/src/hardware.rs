//! Hardwarewerte: what `/status` reports about GPU and CPU (see
//! docs/pc-agent-api.md). Read from Windows itself, without a driver.

use serde::{Deserialize, Serialize};

/// A value that could not be read is `None`, sent as `null`.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Hardware {
    /// The GPU with the most VRAM; `None` if there is none.
    pub gpu: Option<Gpu>,
    pub cpu: Cpu,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Gpu {
    pub load_pct: Option<f32>,
    pub temp_c: Option<f32>,
    pub vram_used_bytes: Option<u64>,
    pub vram_total_bytes: Option<u64>,
    /// 0 while the fan stands still.
    pub fan_rpm: Option<u32>,
    /// Power draw as a share of the card's power limit.
    pub power_pct: Option<f32>,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cpu {
    pub load_pct: Option<f32>,
    pub ram_used_bytes: Option<u64>,
    pub ram_total_bytes: Option<u64>,
}

/// The latest sample. A thread takes one every `SAMPLE` from the first call
/// on, so `/status` never waits for the counters.
#[cfg(windows)]
pub fn read() -> Hardware {
    windows::read()
}

#[cfg(windows)]
mod windows {
    use std::{
        sync::{Mutex, Once},
        time::Duration,
    };

    use windows_sys::Win32::{
        Foundation::FILETIME,
        System::{
            SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
            Threading::GetSystemTimes,
        },
    };

    use super::*;

    /// Shorter than the proxy's round of 5 s, so a sample is never older.
    const SAMPLE: Duration = Duration::from_secs(2);

    static LATEST: Mutex<Option<Hardware>> = Mutex::new(None);
    static START: Once = Once::new();

    pub fn read() -> Hardware {
        START.call_once(|| {
            let mut sampler = Sampler::default();
            *LATEST.lock().unwrap() = Some(sampler.sample());
            std::thread::spawn(move || {
                loop {
                    std::thread::sleep(SAMPLE);
                    let sample = sampler.sample();
                    *LATEST.lock().unwrap() = Some(sample);
                }
            });
        });
        LATEST.lock().unwrap().clone().unwrap_or_default()
    }

    /// What one sample needs from the one before.
    #[derive(Default)]
    struct Sampler {
        /// Idle and busy time of all CPUs at the last sample, in 100 ns.
        cpu_times: Option<(u64, u64)>,
    }

    impl Sampler {
        fn sample(&mut self) -> Hardware {
            let (ram_used_bytes, ram_total_bytes) = ram();
            Hardware {
                gpu: None,
                cpu: Cpu {
                    load_pct: self.cpu_load(),
                    ram_used_bytes,
                    ram_total_bytes,
                },
            }
        }

        /// Load since the last sample; `None` for the first.
        fn cpu_load(&mut self) -> Option<f32> {
            let now = cpu_times()?;
            let before = self.cpu_times.replace(now)?;
            let idle = now.0.saturating_sub(before.0);
            let total = now.1.saturating_sub(before.1);
            (total > 0)
                .then(|| (100.0 * (1.0 - idle as f64 / total as f64)).clamp(0.0, 100.0) as f32)
        }
    }

    fn ticks(t: FILETIME) -> u64 {
        (u64::from(t.dwHighDateTime) << 32) | u64::from(t.dwLowDateTime)
    }

    /// Idle time and total time of all CPUs.
    fn cpu_times() -> Option<(u64, u64)> {
        let zero = FILETIME {
            dwLowDateTime: 0,
            dwHighDateTime: 0,
        };
        let (mut idle, mut kernel, mut user) = (zero, zero, zero);
        // SAFETY: three valid FILETIME outputs.
        if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } == 0 {
            return None;
        }
        // Kernel time includes idle time.
        Some((ticks(idle), ticks(kernel) + ticks(user)))
    }

    fn ram() -> (Option<u64>, Option<u64>) {
        // SAFETY: MEMORYSTATUSEX is plain data; dwLength is set below.
        let mut info: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
        info.dwLength = size_of::<MEMORYSTATUSEX>() as u32;
        // SAFETY: `info` is a valid MEMORYSTATUSEX with dwLength set.
        if unsafe { GlobalMemoryStatusEx(&mut info) } == 0 {
            return (None, None);
        }
        (
            Some(info.ullTotalPhys - info.ullAvailPhys),
            Some(info.ullTotalPhys),
        )
    }
}
