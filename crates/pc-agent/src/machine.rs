//! The machine facts: what the PC is (RAM sticks, graphics cards, CPU,
//! Windows, the drive with the models), read once from Windows itself
//! without a driver. `GET /strata` reports them (see docs/pc-agent-api.md).

pub use strata_pc_protocol::{CpuFacts, GpuFacts, MachineFacts, RamStick, WindowsFacts};

/// The facts of this PC; `data_dir` is the Strata data folder, whose drive
/// counts.
#[cfg(windows)]
pub fn read(data_dir: &std::path::Path) -> MachineFacts {
    let (ram_sticks, ram_slots) = windows::smbios()
        .map(|table| memory_devices(&table))
        .unwrap_or_default();
    MachineFacts {
        ram_sticks,
        ram_slots,
        gpus: windows::gpus(),
        cpu: CpuFacts {
            name: cpu_name(),
            cores: windows::cores(),
            threads: windows::threads(),
            base_mhz: windows::base_mhz(),
            avx2: avx2(),
            avx512: avx512(),
        },
        windows: windows::version(),
        model_drive: windows::drive_kind(data_dir),
    }
}

/// The filled memory devices of an SMBIOS table (type 17) and the number of
/// slots; `table` is the structures without the raw header.
pub fn memory_devices(table: &[u8]) -> (Vec<RamStick>, Option<u32>) {
    let mut sticks = Vec::new();
    let mut slots = 0u32;
    let mut at = 0;
    while at + 4 <= table.len() {
        let kind = table[at];
        let len = table[at + 1] as usize;
        if len < 4 || at + len > table.len() {
            break;
        }
        let s = &table[at..at + len];
        if kind == 17 {
            slots += 1;
            if let Some(stick) = memory_device(s) {
                sticks.push(stick);
            }
        }
        if kind == 127 {
            break;
        }
        // The strings follow the formatted part and end with two NULs.
        let mut end = at + len;
        while end + 1 < table.len() && !(table[end] == 0 && table[end + 1] == 0) {
            end += 1;
        }
        at = end + 2;
    }
    (sticks, (slots > 0).then_some(slots))
}

/// One memory device (type 17); `None` for an empty slot.
fn memory_device(s: &[u8]) -> Option<RamStick> {
    let word = |at: usize| (s.len() >= at + 2).then(|| u16::from_le_bytes([s[at], s[at + 1]]));
    let dword = |at: usize| {
        (s.len() >= at + 4).then(|| u32::from_le_bytes([s[at], s[at + 1], s[at + 2], s[at + 3]]))
    };
    let size_bytes = match word(0x0C)? {
        0 => return None,
        0xFFFF => None,
        0x7FFF => dword(0x1C).map(|mb| u64::from(mb & 0x7FFF_FFFF) << 20),
        size if size & 0x8000 != 0 => Some(u64::from(size & 0x7FFF) << 10),
        size => Some(u64::from(size) << 20),
    };
    let speed = |short: usize, extended: usize| match word(short) {
        Some(0) | None => None,
        Some(0xFFFF) => dword(extended).filter(|v| *v > 0),
        Some(v) => Some(u32::from(v)),
    };
    Some(RamStick {
        size_bytes,
        kind: s.get(0x12).and_then(|t| memory_type(*t)).map(Into::into),
        rated_mts: speed(0x15, 0x54),
        configured_mts: speed(0x20, 0x58),
    })
}

/// SMBIOS memory type names.
fn memory_type(code: u8) -> Option<&'static str> {
    Some(match code {
        0x12 => "DDR",
        0x13 => "DDR2",
        0x18 => "DDR3",
        0x1A => "DDR4",
        0x1B => "LPDDR",
        0x1C => "LPDDR2",
        0x1D => "LPDDR3",
        0x1E => "LPDDR4",
        0x22 => "DDR5",
        0x23 => "LPDDR5",
        _ => return None,
    })
}

/// `Windows 11 Pro`: Windows 11 still names itself Windows 10 in the
/// registry; its builds start at 22000.
pub fn edition(product_name: &str, build: Option<u32>) -> String {
    match product_name.strip_prefix("Windows 10") {
        Some(rest) if build.is_some_and(|b| b >= 22000) => format!("Windows 11{rest}"),
        _ => product_name.to_string(),
    }
}

/// A driver version as Windows shows it: four 16-bit parts.
pub fn driver_version(raw: i64) -> String {
    let v = raw as u64;
    format!(
        "{}.{}.{}.{}",
        v >> 48,
        (v >> 32) & 0xFFFF,
        (v >> 16) & 0xFFFF,
        v & 0xFFFF
    )
}

/// `nvme`, `ssd` or `hdd` from the drive's bus and whether it seeks.
pub fn drive_kind(nvme: bool, seek_penalty: Option<bool>) -> Option<String> {
    let kind = match (nvme, seek_penalty) {
        (true, _) => "nvme",
        (false, Some(false)) => "ssd",
        (false, Some(true)) => "hdd",
        (false, None) => return None,
    };
    Some(kind.into())
}

/// The CPU's name as it gives it itself.
#[cfg_attr(not(windows), allow(dead_code))]
fn cpu_name() -> Option<String> {
    #[cfg(target_arch = "x86_64")]
    {
        use std::arch::x86_64::__cpuid;
        if __cpuid(0x8000_0000).eax < 0x8000_0004 {
            return None;
        }
        let mut bytes = Vec::with_capacity(48);
        for leaf in 0x8000_0002..=0x8000_0004u32 {
            let r = __cpuid(leaf);
            for reg in [r.eax, r.ebx, r.ecx, r.edx] {
                bytes.extend_from_slice(&reg.to_le_bytes());
            }
        }
        let name = String::from_utf8_lossy(&bytes);
        let name = name.trim_matches(char::from(0)).trim();
        (!name.is_empty()).then(|| name.to_string())
    }
    #[cfg(not(target_arch = "x86_64"))]
    None
}

#[cfg(target_arch = "x86_64")]
#[cfg_attr(not(windows), allow(dead_code))]
fn avx2() -> Option<bool> {
    Some(std::arch::is_x86_feature_detected!("avx2"))
}

#[cfg(target_arch = "x86_64")]
#[cfg_attr(not(windows), allow(dead_code))]
fn avx512() -> Option<bool> {
    Some(std::arch::is_x86_feature_detected!("avx512f"))
}

#[cfg(not(target_arch = "x86_64"))]
#[cfg_attr(not(windows), allow(dead_code))]
fn avx2() -> Option<bool> {
    None
}

#[cfg(not(target_arch = "x86_64"))]
#[cfg_attr(not(windows), allow(dead_code))]
fn avx512() -> Option<bool> {
    None
}

#[cfg(windows)]
mod windows {
    use std::{os::windows::ffi::OsStrExt, path::Path};

    use windows_sys::{
        Wdk::Graphics::Direct3D::{
            D3DKMT_ADAPTERADDRESS, D3DKMT_ADAPTERREGISTRYINFO, D3DKMT_SEGMENTSIZEINFO,
            D3DKMT_UMD_DRIVER_VERSION, KMTQAITYPE_ADAPTERADDRESS, KMTQAITYPE_ADAPTERREGISTRYINFO,
            KMTQAITYPE_GETSEGMENTSIZE, KMTQAITYPE_UMD_DRIVER_VERSION,
        },
        Win32::{
            Devices::{
                DeviceAndDriverInstallation::{
                    DIGCF_PRESENT, GUID_DEVCLASS_DISPLAY, SP_DEVINFO_DATA,
                    SetupDiDestroyDeviceInfoList, SetupDiEnumDeviceInfo, SetupDiGetClassDevsW,
                    SetupDiGetDevicePropertyW,
                },
                Properties::{DEVPKEY_Device_Address, DEVPKEY_Device_BusNumber, DEVPROPTYPE},
            },
            Foundation::{CloseHandle, DEVPROPKEY, ERROR_SUCCESS, INVALID_HANDLE_VALUE, MAX_PATH},
            Storage::FileSystem::{
                BusTypeNvme, CreateFileW, FILE_SHARE_READ, FILE_SHARE_WRITE,
                GetVolumeNameForVolumeMountPointW, GetVolumePathNameW, OPEN_EXISTING,
            },
            System::{
                IO::DeviceIoControl,
                Ioctl::{
                    DEVICE_SEEK_PENALTY_DESCRIPTOR, IOCTL_STORAGE_QUERY_PROPERTY,
                    PropertyStandardQuery, STORAGE_ADAPTER_DESCRIPTOR, STORAGE_PROPERTY_QUERY,
                    StorageAdapterProperty, StorageDeviceSeekPenaltyProperty,
                },
                Power::{
                    CallNtPowerInformation, PROCESSOR_POWER_INFORMATION, ProcessorInformation,
                },
                Registry::{HKEY_LOCAL_MACHINE, RRF_RT_REG_DWORD, RRF_RT_REG_SZ, RegGetValueW},
                SystemInformation::{
                    GetLogicalProcessorInformationEx, GetSystemFirmwareTable, RSMB,
                    RelationProcessorCore, SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX,
                },
                Threading::{ALL_PROCESSOR_GROUPS, GetActiveProcessorCount},
            },
        },
    };

    use super::*;
    use crate::d3dkmt::{query, with_adapters};

    /// The PCI Express link properties (devpkey.h), not in the crate's
    /// device property module.
    const PCI_LINK: windows_sys::core::GUID =
        windows_sys::core::GUID::from_u128(0x3ab22e31_8264_4b4e_9af5_a8d2d8e33e62);
    const fn pci_key(pid: u32) -> DEVPROPKEY {
        DEVPROPKEY {
            fmtid: PCI_LINK,
            pid,
        }
    }
    const CURRENT_LINK_SPEED: DEVPROPKEY = pci_key(9);
    const CURRENT_LINK_WIDTH: DEVPROPKEY = pci_key(10);
    const MAX_LINK_SPEED: DEVPROPKEY = pci_key(11);
    const MAX_LINK_WIDTH: DEVPROPKEY = pci_key(12);

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(Some(0)).collect()
    }

    fn from_wide(w: &[u16]) -> Option<String> {
        let end = w.iter().position(|c| *c == 0).unwrap_or(w.len());
        let s = String::from_utf16_lossy(&w[..end]).trim().to_string();
        (!s.is_empty()).then_some(s)
    }

    /// The SMBIOS structures, without the raw header.
    pub fn smbios() -> Option<Vec<u8>> {
        // SAFETY: a null buffer asks for the size only.
        let size = unsafe { GetSystemFirmwareTable(RSMB, 0, std::ptr::null_mut(), 0) };
        if size < 8 {
            return None;
        }
        let mut raw = vec![0u8; size as usize];
        // SAFETY: `raw` holds `size` bytes.
        let got = unsafe { GetSystemFirmwareTable(RSMB, 0, raw.as_mut_ptr(), size) };
        if got != size {
            return None;
        }
        // RawSMBIOSData: 4 bytes of versions, the length, then the table.
        let len = u32::from_le_bytes([raw[4], raw[5], raw[6], raw[7]]) as usize;
        raw.get(8..8 + len).map(<[u8]>::to_vec)
    }

    /// Physical cores, one entry per core.
    pub fn cores() -> Option<u32> {
        let mut len = 0u32;
        // SAFETY: a null buffer asks for the size only.
        unsafe {
            GetLogicalProcessorInformationEx(RelationProcessorCore, std::ptr::null_mut(), &mut len)
        };
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        // SAFETY: `buf` holds `len` bytes.
        let ok = unsafe {
            GetLogicalProcessorInformationEx(
                RelationProcessorCore,
                buf.as_mut_ptr()
                    .cast::<SYSTEM_LOGICAL_PROCESSOR_INFORMATION_EX>(),
                &mut len,
            )
        };
        if ok == 0 {
            return None;
        }
        let (mut at, mut cores) = (0usize, 0u32);
        // Each entry starts with its relationship and its size.
        while at + 8 <= len as usize {
            let size = u32::from_le_bytes([buf[at + 4], buf[at + 5], buf[at + 6], buf[at + 7]]);
            if size == 0 {
                break;
            }
            cores += 1;
            at += size as usize;
        }
        (cores > 0).then_some(cores)
    }

    /// Logical processors in every processor group, whatever this
    /// process's affinity.
    pub fn threads() -> Option<u32> {
        // SAFETY: no arguments besides the group selector.
        let n = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) };
        (n > 0).then_some(n)
    }

    /// The base clock: Windows' `MaxMhz` is the rated clock, not the boost.
    pub fn base_mhz() -> Option<u32> {
        let n = threads()? as usize;
        let mut info: Vec<PROCESSOR_POWER_INFORMATION> = (0..n)
            .map(|_| PROCESSOR_POWER_INFORMATION {
                Number: 0,
                MaxMhz: 0,
                CurrentMhz: 0,
                MhzLimit: 0,
                MaxIdleState: 0,
                CurrentIdleState: 0,
            })
            .collect();
        let bytes = (n * size_of::<PROCESSOR_POWER_INFORMATION>()) as u32;
        // SAFETY: `info` holds `bytes` bytes.
        let status = unsafe {
            CallNtPowerInformation(
                ProcessorInformation,
                std::ptr::null(),
                0,
                info.as_mut_ptr().cast(),
                bytes,
            )
        };
        if status != 0 {
            return None;
        }
        info.iter().map(|p| p.MaxMhz).max().filter(|m| *m > 0)
    }

    fn reg_string(key: &str, value: &str) -> Option<String> {
        let (key, value) = (wide(key), wide(value));
        let mut buf = [0u16; 256];
        let mut len = (buf.len() * 2) as u32;
        // SAFETY: both names are NUL-terminated; `buf` holds `len` bytes.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_SZ,
                std::ptr::null_mut(),
                buf.as_mut_ptr().cast(),
                &mut len,
            )
        };
        (status == ERROR_SUCCESS).then(|| from_wide(&buf)).flatten()
    }

    fn reg_dword(key: &str, value: &str) -> Option<u32> {
        let (key, value) = (wide(key), wide(value));
        let mut out = 0u32;
        let mut len = 4u32;
        // SAFETY: both names are NUL-terminated; `out` holds 4 bytes.
        let status = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                key.as_ptr(),
                value.as_ptr(),
                RRF_RT_REG_DWORD,
                std::ptr::null_mut(),
                (&mut out as *mut u32).cast(),
                &mut len,
            )
        };
        (status == ERROR_SUCCESS).then_some(out)
    }

    pub fn version() -> WindowsFacts {
        const KEY: &str = r"SOFTWARE\Microsoft\Windows NT\CurrentVersion";
        let build = reg_string(KEY, "CurrentBuild");
        let number = build.as_deref().and_then(|b| b.parse().ok());
        WindowsFacts {
            edition: reg_string(KEY, "ProductName").map(|p| edition(&p, number)),
            version: reg_string(KEY, "DisplayVersion"),
            build: build.map(|b| match reg_dword(KEY, "UBR") {
                Some(ubr) => format!("{b}.{ubr}"),
                None => b,
            }),
        }
    }

    /// `nvme`, `ssd` or `hdd` for the drive that holds `path`.
    pub fn drive_kind(path: &Path) -> Option<String> {
        let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut root = [0u16; MAX_PATH as usize];
        // SAFETY: `path` is NUL-terminated; `root` holds MAX_PATH characters.
        if unsafe { GetVolumePathNameW(path.as_ptr(), root.as_mut_ptr(), MAX_PATH) } == 0 {
            return None;
        }
        // `C:\` or a folder a volume is mounted into → `\\?\Volume{…}\`;
        // without the last backslash it opens the volume itself.
        let mut volume = [0u16; 64];
        // SAFETY: `root` is NUL-terminated; `volume` holds 64 characters.
        if unsafe {
            GetVolumeNameForVolumeMountPointW(
                root.as_ptr(),
                volume.as_mut_ptr(),
                volume.len() as u32,
            )
        } == 0
        {
            return None;
        }
        let volume = from_wide(&volume)?;
        let device = wide(volume.trim_end_matches('\\'));
        // SAFETY: `device` is NUL-terminated; no access is asked, only queries.
        let handle = unsafe {
            CreateFileW(
                device.as_ptr(),
                0,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                std::ptr::null(),
                OPEN_EXISTING,
                0,
                std::ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return None;
        }
        let query = |id| STORAGE_PROPERTY_QUERY {
            PropertyId: id,
            QueryType: PropertyStandardQuery,
            AdditionalParameters: [0],
        };
        // SAFETY: zeroed descriptors are valid; the handle is open.
        let mut adapter: STORAGE_ADAPTER_DESCRIPTOR = unsafe { std::mem::zeroed() };
        let mut seek: DEVICE_SEEK_PENALTY_DESCRIPTOR = unsafe { std::mem::zeroed() };
        let ask = |id, out: *mut core::ffi::c_void, size: usize| {
            let q = query(id);
            let mut got = 0u32;
            // SAFETY: `q` and `out` are valid for their sizes; the handle is open.
            unsafe {
                DeviceIoControl(
                    handle,
                    IOCTL_STORAGE_QUERY_PROPERTY,
                    (&q as *const STORAGE_PROPERTY_QUERY).cast(),
                    size_of::<STORAGE_PROPERTY_QUERY>() as u32,
                    out,
                    size as u32,
                    &mut got,
                    std::ptr::null_mut(),
                ) != 0
            }
        };
        let nvme = ask(
            StorageAdapterProperty,
            (&mut adapter as *mut STORAGE_ADAPTER_DESCRIPTOR).cast(),
            size_of::<STORAGE_ADAPTER_DESCRIPTOR>(),
        ) && i32::from(adapter.BusType) == BusTypeNvme;
        let seek_penalty = ask(
            StorageDeviceSeekPenaltyProperty,
            (&mut seek as *mut DEVICE_SEEK_PENALTY_DESCRIPTOR).cast(),
            size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>(),
        )
        .then_some(seek.IncursSeekPenalty);
        // SAFETY: opened above, closed once.
        unsafe { CloseHandle(handle) };
        super::drive_kind(nvme, seek_penalty)
    }

    /// Every adapter with VRAM of its own, the most first, with its PCIe
    /// link from the device it sits on.
    pub fn gpus() -> Vec<GpuFacts> {
        let links = pci_links();
        let mut gpus = with_adapters(|adapters| {
            adapters
                .iter()
                .filter_map(|info| gpu(info.hAdapter, &links))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
        gpus.sort_by_key(|g| std::cmp::Reverse(g.vram_bytes));
        gpus
    }

    /// One adapter's facts; `None` without VRAM of its own.
    fn gpu(h: u32, links: &[Link]) -> Option<GpuFacts> {
        let mut size = D3DKMT_SEGMENTSIZEINFO::default();
        // SAFETY: the handle comes from the enumeration and is open.
        if !unsafe { query(h, KMTQAITYPE_GETSEGMENTSIZE, &mut size) }
            || size.DedicatedVideoMemorySize == 0
        {
            return None;
        }
        let mut registry = D3DKMT_ADAPTERREGISTRYINFO::default();
        // SAFETY: as above.
        let name = unsafe { query(h, KMTQAITYPE_ADAPTERREGISTRYINFO, &mut registry) }
            .then(|| from_wide(&registry.AdapterString))
            .flatten();
        let mut version = D3DKMT_UMD_DRIVER_VERSION { DriverVersion: 0 };
        // SAFETY: as above.
        let driver_version = (unsafe { query(h, KMTQAITYPE_UMD_DRIVER_VERSION, &mut version) }
            && version.DriverVersion != 0)
            .then(|| driver_version(version.DriverVersion));
        let mut address = D3DKMT_ADAPTERADDRESS {
            BusNumber: 0,
            DeviceNumber: 0,
            FunctionNumber: 0,
        };
        // SAFETY: as above.
        let link = unsafe { query(h, KMTQAITYPE_ADAPTERADDRESS, &mut address) }
            .then(|| {
                links.iter().find(|l| {
                    l.bus == address.BusNumber
                        && l.address == (address.DeviceNumber << 16 | address.FunctionNumber)
                })
            })
            .flatten();
        Some(GpuFacts {
            name,
            vram_bytes: Some(size.DedicatedVideoMemorySize),
            driver_version,
            pcie_max_gen: link.and_then(|l| l.max_gen),
            pcie_max_lanes: link.and_then(|l| l.max_lanes),
            pcie_gen: link.and_then(|l| l.generation),
            pcie_lanes: link.and_then(|l| l.lanes),
        })
    }

    /// A display device's place on the bus and its PCIe link.
    struct Link {
        bus: u32,
        address: u32,
        generation: Option<u8>,
        lanes: Option<u8>,
        max_gen: Option<u8>,
        max_lanes: Option<u8>,
    }

    /// The display devices with their PCIe links, from Setup API.
    fn pci_links() -> Vec<Link> {
        // SAFETY: the class GUID is valid; no enumerator or window.
        let set = unsafe {
            SetupDiGetClassDevsW(
                &GUID_DEVCLASS_DISPLAY,
                std::ptr::null(),
                std::ptr::null_mut(),
                DIGCF_PRESENT,
            )
        };
        if set as isize == -1 {
            return Vec::new();
        }
        let mut links = Vec::new();
        for i in 0.. {
            let mut dev = SP_DEVINFO_DATA {
                cbSize: size_of::<SP_DEVINFO_DATA>() as u32,
                ClassGuid: windows_sys::core::GUID::from_u128(0),
                DevInst: 0,
                Reserved: 0,
            };
            // SAFETY: `set` is open; `dev` has its size set.
            if unsafe { SetupDiEnumDeviceInfo(set, i, &mut dev) } == 0 {
                break;
            }
            let prop = |key: &DEVPROPKEY| -> Option<u32> {
                let mut kind: DEVPROPTYPE = 0;
                let mut out = 0u32;
                // SAFETY: `out` holds 4 bytes, the size of every property read here.
                let ok = unsafe {
                    SetupDiGetDevicePropertyW(
                        set,
                        &dev,
                        key,
                        &mut kind,
                        (&mut out as *mut u32).cast(),
                        4,
                        std::ptr::null_mut(),
                        0,
                    )
                };
                (ok != 0).then_some(out)
            };
            let (Some(bus), Some(address)) = (
                prop(&DEVPKEY_Device_BusNumber),
                prop(&DEVPKEY_Device_Address),
            ) else {
                continue;
            };
            let small = |v: Option<u32>| v.and_then(|v| u8::try_from(v).ok()).filter(|v| *v > 0);
            links.push(Link {
                bus,
                address,
                generation: small(prop(&CURRENT_LINK_SPEED)),
                lanes: small(prop(&CURRENT_LINK_WIDTH)),
                max_gen: small(prop(&MAX_LINK_SPEED)),
                max_lanes: small(prop(&MAX_LINK_WIDTH)),
            });
        }
        // SAFETY: opened above, destroyed once.
        unsafe { SetupDiDestroyDeviceInfoList(set) };
        links
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A type-17 structure of SMBIOS 3.3 (length 0x5C) with no strings.
    fn device(size: u16, kind: u8, speed: u16, configured: u16, extended: u32) -> Vec<u8> {
        let mut s = vec![0u8; 0x5C];
        s[0] = 17;
        s[1] = 0x5C;
        s[0x0C..0x0E].copy_from_slice(&size.to_le_bytes());
        s[0x12] = kind;
        s[0x15..0x17].copy_from_slice(&speed.to_le_bytes());
        s[0x1C..0x20].copy_from_slice(&extended.to_le_bytes());
        s[0x20..0x22].copy_from_slice(&configured.to_le_bytes());
        s.extend_from_slice(&[0, 0]);
        s
    }

    #[test]
    fn reads_filled_and_empty_slots() {
        let mut table = Vec::new();
        // A BIOS structure with a string before it.
        table.extend_from_slice(&[0, 4, 0, 0]);
        table.extend_from_slice(b"Vendor\0\0");
        table.extend(device(16 * 1024, 0x22, 6000, 5600, 0));
        table.extend(device(0, 0, 0, 0, 0));
        table.extend(device(0x7FFF, 0x1A, 3200, 3200, 64 * 1024));
        table.extend_from_slice(&[127, 4, 0, 0, 0, 0]);
        let (sticks, slots) = memory_devices(&table);
        assert_eq!(slots, Some(3));
        assert_eq!(
            sticks,
            vec![
                RamStick {
                    size_bytes: Some(16 << 30),
                    kind: Some("DDR5".into()),
                    rated_mts: Some(6000),
                    configured_mts: Some(5600),
                },
                RamStick {
                    size_bytes: Some(64 << 30),
                    kind: Some("DDR4".into()),
                    rated_mts: Some(3200),
                    configured_mts: Some(3200),
                },
            ]
        );
    }

    #[test]
    fn an_old_short_structure_has_no_speeds() {
        // SMBIOS 2.1: 0x15 bytes, size in KB.
        let mut s = vec![0u8; 0x15];
        s[0] = 17;
        s[1] = 0x15;
        s[0x0C..0x0E].copy_from_slice(&(0x8000u16 | 512).to_le_bytes());
        s[0x12] = 0x13;
        s.extend_from_slice(&[0, 0]);
        let (sticks, slots) = memory_devices(&s);
        assert_eq!(slots, Some(1));
        assert_eq!(sticks[0].size_bytes, Some(512 << 10));
        assert_eq!(sticks[0].kind.as_deref(), Some("DDR2"));
        assert_eq!(sticks[0].rated_mts, None);
        assert_eq!(sticks[0].configured_mts, None);
    }

    #[test]
    fn a_broken_table_gives_what_came_before() {
        let mut table = device(16 * 1024, 0x22, 4800, 4800, 0);
        table.extend_from_slice(&[17, 0x5C, 0]);
        let (sticks, slots) = memory_devices(&table);
        assert_eq!(sticks.len(), 1);
        assert_eq!(slots, Some(1));
    }

    #[test]
    fn windows_11_names_itself_windows_10() {
        assert_eq!(edition("Windows 10 Pro", Some(26100)), "Windows 11 Pro");
        assert_eq!(edition("Windows 10 Pro", Some(19045)), "Windows 10 Pro");
        assert_eq!(edition("Windows 10 Home", None), "Windows 10 Home");
    }

    #[test]
    fn driver_version_in_four_parts() {
        let raw = (32i64 << 48) | (15 << 16) | 6094;
        assert_eq!(driver_version(raw), "32.0.15.6094");
    }

    #[test]
    fn drive_kinds() {
        assert_eq!(drive_kind(true, Some(false)).as_deref(), Some("nvme"));
        assert_eq!(drive_kind(false, Some(false)).as_deref(), Some("ssd"));
        assert_eq!(drive_kind(false, Some(true)).as_deref(), Some("hdd"));
        assert_eq!(drive_kind(false, None), None);
    }
}
