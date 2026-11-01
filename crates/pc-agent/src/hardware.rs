//! Hardware readings: what `/status` reports about GPU and CPU (see
//! docs/pc-agent-api.md). Read from Windows itself; only the GPU's watts
//! come from AMD's driver library (`crate::adl`).

pub use strata_pc_protocol::{Cpu, Gpu, Hardware};

/// The latest sample, without waiting. A thread takes one every `SAMPLE`
/// while `/status` is asked; the first call starts it and gets no values.
#[cfg(windows)]
pub fn read() -> Hardware {
    windows::read()
}

/// Dedicated VRAM of the biggest card; 0 without one.
#[cfg(windows)]
pub fn vram_bytes() -> u64 {
    windows::vram_bytes()
}

/// One sensor of ADL's PMLog, as `ADLSingleSensorData` in ADL SDK's
/// `adl_structures.h`.
#[repr(C)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AdlSensor {
    pub supported: i32,
    pub value: i32,
}

/// `PMLOG_ASIC_POWER` in `ADLSensorType`, ADL SDK's `adl_defines.h`
/// (GPUOpen display-library): "Current ASIC power draw in Watt".
pub const PMLOG_ASIC_POWER: usize = 23;

/// The GPU chip's power in watts from PMLog's sensors: unknown when the
/// sensor is missing, unsupported or reads 0, as the driver reports 0 for
/// what it does not know.
pub fn asic_power(sensors: &[AdlSensor]) -> Option<f32> {
    sensors
        .get(PMLOG_ASIC_POWER)
        .filter(|s| s.supported != 0 && s.value > 0)
        .map(|s| s.value as f32)
}

/// Where a card sits on the PCI bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PciAddress {
    pub bus: u32,
    pub device: u32,
    pub function: u32,
}

/// One of ADL's adapters: its index for ADL's calls and its PCI address.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AdlAdapter {
    pub index: i32,
    pub at: PciAddress,
}

/// ADL's index of the card at `at`; ADL lists a card once per display
/// output, all with its address, so the first one counts.
pub fn adl_adapter_at(adapters: &[AdlAdapter], at: PciAddress) -> Option<i32> {
    adapters.iter().find(|a| a.at == at).map(|a| a.index)
}

#[cfg(windows)]
mod windows {
    use std::{
        sync::{
            Mutex,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    };

    use windows_sys::{
        Wdk::Graphics::Direct3D::{
            D3DKMT_ADAPTER_PERFDATA, D3DKMT_ADAPTER_PERFDATACAPS, D3DKMT_ADAPTERADDRESS,
            D3DKMT_ADAPTERINFO, D3DKMT_SEGMENTSIZEINFO, KMTQAITYPE_ADAPTERADDRESS,
            KMTQAITYPE_ADAPTERPERFDATA, KMTQAITYPE_ADAPTERPERFDATA_CAPS, KMTQAITYPE_GETSEGMENTSIZE,
        },
        Win32::{
            Foundation::{FILETIME, LUID},
            System::{
                Performance::{
                    PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W,
                    PDH_FMT_DOUBLE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA, PdhAddEnglishCounterW,
                    PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
                    PdhOpenQueryW,
                },
                SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX},
                Threading::GetSystemTimes,
            },
        },
    };

    use super::*;
    use crate::d3dkmt::{query, with_adapters};

    /// Shorter than the proxy's round of 5 s, so a sample is never older.
    const SAMPLE: Duration = Duration::from_secs(2);

    /// The thread stops after this long without a call.
    const IDLE: Duration = Duration::from_secs(30);

    static LATEST: Mutex<Option<Hardware>> = Mutex::new(None);
    static LAST_READ: Mutex<Option<Instant>> = Mutex::new(None);
    static RUNNING: AtomicBool = AtomicBool::new(false);

    pub fn read() -> Hardware {
        *LAST_READ.lock().unwrap() = Some(Instant::now());
        if !RUNNING.swap(true, Ordering::SeqCst) {
            std::thread::spawn(sample_while_asked);
        }
        LATEST.lock().unwrap().clone().unwrap_or_default()
    }

    fn sample_while_asked() {
        let mut sampler = Sampler::default();
        loop {
            let sample = sampler.sample();
            *LATEST.lock().unwrap() = Some(sample);
            std::thread::sleep(SAMPLE);
            if LAST_READ.lock().unwrap().is_none_or(|t| t.elapsed() > IDLE) {
                // Nothing stale for the next caller.
                *LATEST.lock().unwrap() = None;
                RUNNING.store(false, Ordering::SeqCst);
                return;
            }
        }
    }

    /// What one sample needs from the one before.
    #[derive(Default)]
    struct Sampler {
        /// Idle and busy time of all CPUs at the last sample, in 100 ns.
        cpu_times: Option<(u64, u64)>,
        /// Opened with the first sample; again after a failure.
        counters: Option<Counters>,
    }

    impl Sampler {
        fn sample(&mut self) -> Hardware {
            let (ram_used_bytes, ram_total_bytes) = ram();
            Hardware {
                gpu: self.gpu(),
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

        fn gpu(&mut self) -> Option<Gpu> {
            let adapter = biggest_adapter()?;
            if self.counters.is_none() {
                self.counters = Counters::open();
            }
            let (engines, memory) = self
                .counters
                .as_ref()
                .map(Counters::collect)
                .unwrap_or_default();
            if engines.is_empty() && memory.is_empty() {
                // Opened again with the next sample.
                self.counters = None;
            }
            // Counter instances name the adapter like `luid_0x00000000_0x0000C9A3_phys_0`.
            let tag = format!(
                "luid_0x{:08x}_0x{:08x}",
                adapter.luid.HighPart as u32, adapter.luid.LowPart
            );
            Some(Gpu {
                load_pct: engine_load(&engines, &tag),
                vram_used_bytes: memory
                    .iter()
                    .find(|(name, _)| name.to_ascii_lowercase().starts_with(&tag))
                    .map(|(_, bytes)| *bytes as u64),
                vram_total_bytes: Some(adapter.vram_total),
                temp_c: adapter.temp_c,
                fan_rpm: adapter.fan_rpm,
                power_pct: adapter.power_pct,
                power_watts: adapter.at.and_then(crate::adl::power_watts),
            })
        }
    }

    /// Each instance of a wildcard counter with its value.
    type Instances = Vec<(String, f64)>;

    /// Load of the busiest engine (3D, copy, video …) of the adapter, each
    /// engine summed over all processes, as Task Manager shows it.
    fn engine_load(engines: &Instances, tag: &str) -> Option<f32> {
        // Engine name and its load summed over processes.
        let mut per_engine: Vec<(String, f64)> = Vec::new();
        for (name, value) in engines {
            // `pid_1234_luid_0x…_phys_0_eng_0_engtype_3D`: the part from the
            // LUID on names the engine.
            let name = name.to_ascii_lowercase();
            let Some(start) = name.find(tag) else {
                continue;
            };
            let engine = &name[start..];
            match per_engine.iter_mut().find(|(e, _)| e == engine) {
                Some((_, sum)) => *sum += value,
                None => per_engine.push((engine.to_string(), *value)),
            }
        }
        per_engine
            .iter()
            .map(|(_, sum)| sum.clamp(0.0, 100.0) as f32)
            .reduce(f32::max)
    }

    /// Read once: a card's VRAM does not change while the service runs.
    pub fn vram_bytes() -> u64 {
        static VRAM: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
        *VRAM.get_or_init(|| biggest_adapter().map_or(0, |a| a.vram_total))
    }

    struct Adapter {
        luid: LUID,
        vram_total: u64,
        temp_c: Option<f32>,
        fan_rpm: Option<u32>,
        power_pct: Option<f32>,
        /// `None` if the driver does not say.
        at: Option<PciAddress>,
    }

    /// The adapter with the most dedicated VRAM; an integrated chip has
    /// little or none.
    fn biggest_adapter() -> Option<Adapter> {
        with_adapters(|adapters| {
            let mut biggest: Option<(&D3DKMT_ADAPTERINFO, u64)> = None;
            for info in adapters {
                let mut size = D3DKMT_SEGMENTSIZEINFO::default();
                // SAFETY: the handle comes from the enumeration.
                if unsafe { query(info.hAdapter, KMTQAITYPE_GETSEGMENTSIZE, &mut size) }
                    && size.DedicatedVideoMemorySize > 0
                    && biggest.is_none_or(|(_, vram)| size.DedicatedVideoMemorySize > vram)
                {
                    biggest = Some((info, size.DedicatedVideoMemorySize));
                }
            }
            biggest.map(|(info, vram_total)| {
                // SAFETY: the handle is still open.
                let (temp_c, fan_rpm, power_pct) = unsafe { perf(info.hAdapter) };
                let mut address = D3DKMT_ADAPTERADDRESS {
                    BusNumber: 0,
                    DeviceNumber: 0,
                    FunctionNumber: 0,
                };
                // SAFETY: as above.
                let at = unsafe { query(info.hAdapter, KMTQAITYPE_ADAPTERADDRESS, &mut address) }
                    .then_some(PciAddress {
                        bus: address.BusNumber,
                        device: address.DeviceNumber,
                        function: address.FunctionNumber,
                    });
                Adapter {
                    luid: info.AdapterLuid,
                    vram_total,
                    temp_c,
                    fan_rpm,
                    power_pct,
                    at,
                }
            })
        })
        .flatten()
    }

    /// Temperature, fan and power, from the data Task Manager shows. The
    /// driver reports 0 for what it does not know; only for the fan 0 is
    /// also a real value, so it counts if the driver names a fan maximum.
    ///
    /// # Safety
    /// `handle` is an open adapter.
    unsafe fn perf(handle: u32) -> (Option<f32>, Option<u32>, Option<f32>) {
        let mut data = D3DKMT_ADAPTER_PERFDATA::default();
        // SAFETY: the caller's handle; PhysicalAdapterIndex 0 is the first GPU behind it.
        if !unsafe { query(handle, KMTQAITYPE_ADAPTERPERFDATA, &mut data) } {
            return (None, None, None);
        }
        let mut caps = D3DKMT_ADAPTER_PERFDATACAPS::default();
        // SAFETY: as above.
        let fan_known = unsafe { query(handle, KMTQAITYPE_ADAPTERPERFDATA_CAPS, &mut caps) }
            && caps.MaxFanRPM > 0;
        (
            // Tenths of a degree Celsius.
            (data.Temperature > 0).then(|| data.Temperature as f32 / 10.0),
            (data.FanRPM > 0 || fan_known).then_some(data.FanRPM),
            // Tenths of a percent of the power limit.
            (data.Power > 0).then(|| data.Power as f32 / 10.0),
        )
    }

    /// The performance counters for GPU load and VRAM in use, by their
    /// English names, so a German Windows finds them too.
    struct Counters {
        query: PDH_HQUERY,
        /// Either may be missing; the other still counts.
        engines: Option<PDH_HCOUNTER>,
        memory: Option<PDH_HCOUNTER>,
    }

    // SAFETY: PDH handles may be used from any thread; only the sampler's
    // thread uses these.
    unsafe impl Send for Counters {}

    impl Counters {
        fn open() -> Option<Self> {
            let mut query = std::ptr::null_mut();
            // SAFETY: a null data source means live data.
            if unsafe { PdhOpenQueryW(std::ptr::null(), 0, &mut query) } != 0 {
                return None;
            }
            let add = |path: &str| {
                let wide: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
                let mut counter = std::ptr::null_mut();
                // SAFETY: `wide` is NUL-terminated; `query` is open.
                (unsafe { PdhAddEnglishCounterW(query, wide.as_ptr(), 0, &mut counter) } == 0)
                    .then_some(counter)
            };
            let engines = add(r"\GPU Engine(*)\Utilization Percentage");
            let memory = add(r"\GPU Adapter Memory(*)\Dedicated Usage");
            if engines.is_none() && memory.is_none() {
                // SAFETY: opened above, closed once.
                unsafe { PdhCloseQuery(query) };
                return None;
            }
            Some(Self {
                query,
                engines,
                memory,
            })
        }

        /// Engine load per instance and VRAM in use per adapter, since the
        /// last collection.
        fn collect(&self) -> (Instances, Instances) {
            // SAFETY: the query is open.
            if unsafe { PdhCollectQueryData(self.query) } != 0 {
                return (Vec::new(), Vec::new());
            }
            (
                self.engines.map(values).unwrap_or_default(),
                self.memory.map(values).unwrap_or_default(),
            )
        }
    }

    impl Drop for Counters {
        fn drop(&mut self) {
            // SAFETY: opened in `open`, closed once.
            unsafe { PdhCloseQuery(self.query) };
        }
    }

    /// Every instance of a wildcard counter with its value; empty if the
    /// instances changed between the two calls.
    fn values(counter: PDH_HCOUNTER) -> Instances {
        let (mut size, mut count) = (0u32, 0u32);
        // SAFETY: a null buffer asks for the size.
        let status = unsafe {
            PdhGetFormattedCounterArrayW(
                counter,
                PDH_FMT_DOUBLE,
                &mut size,
                &mut count,
                std::ptr::null_mut(),
            )
        };
        if status != PDH_MORE_DATA {
            return Vec::new();
        }
        // u64 for the alignment of the items at its start.
        let mut buffer = vec![0u64; (size as usize).div_ceil(8)];
        let items = buffer.as_mut_ptr().cast::<PDH_FMT_COUNTERVALUE_ITEM_W>();
        // SAFETY: `buffer` holds `size` bytes.
        if unsafe {
            PdhGetFormattedCounterArrayW(counter, PDH_FMT_DOUBLE, &mut size, &mut count, items)
        } != 0
        {
            return Vec::new();
        }
        // SAFETY: PDH wrote `count` items, their names point into `buffer`.
        let items = unsafe { std::slice::from_raw_parts(items, count as usize) };
        items
            .iter()
            .filter(|i| {
                matches!(
                    i.FmtValue.CStatus,
                    PDH_CSTATUS_VALID_DATA | PDH_CSTATUS_NEW_DATA
                )
            })
            .map(|i| {
                // SAFETY: `szName` is NUL-terminated UTF-16 inside `buffer`;
                // the value was asked for as a double.
                let (name, value) = unsafe {
                    let len = (0..).take_while(|&n| *i.szName.add(n) != 0).count();
                    (
                        String::from_utf16_lossy(std::slice::from_raw_parts(i.szName, len)),
                        i.FmtValue.Anonymous.doubleValue,
                    )
                };
                (name, value)
            })
            .collect()
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

#[cfg(test)]
mod tests {
    use super::*;

    fn sensors_with_asic_power(supported: i32, value: i32) -> Vec<AdlSensor> {
        let mut sensors = vec![
            AdlSensor {
                supported: 1,
                value: 77,
            };
            256
        ];
        sensors[PMLOG_ASIC_POWER] = AdlSensor { supported, value };
        sensors
    }

    #[test]
    fn asic_power_is_the_supported_sensors_value_in_watts() {
        assert_eq!(asic_power(&sensors_with_asic_power(1, 182)), Some(182.0));
    }

    #[test]
    fn asic_power_of_0_is_unknown() {
        assert_eq!(asic_power(&sensors_with_asic_power(1, 0)), None);
    }

    #[test]
    fn an_unsupported_asic_power_sensor_is_unknown() {
        assert_eq!(asic_power(&sensors_with_asic_power(0, 182)), None);
    }

    fn at(bus: u32, device: u32, function: u32) -> PciAddress {
        PciAddress {
            bus,
            device,
            function,
        }
    }

    fn adapters() -> Vec<AdlAdapter> {
        vec![
            AdlAdapter {
                index: 0,
                at: at(3, 0, 0),
            },
            AdlAdapter {
                index: 1,
                at: at(3, 0, 0),
            },
            AdlAdapter {
                index: 2,
                at: at(12, 0, 0),
            },
            AdlAdapter {
                index: 3,
                at: at(12, 0, 1),
            },
        ]
    }

    #[test]
    fn the_adl_adapter_is_the_one_at_the_gpus_pci_address() {
        assert_eq!(adl_adapter_at(&adapters(), at(12, 0, 0)), Some(2));
    }

    #[test]
    fn no_adl_adapter_at_the_gpus_pci_address_means_no_watts() {
        assert_eq!(adl_adapter_at(&adapters(), at(7, 0, 0)), None);
    }
}
