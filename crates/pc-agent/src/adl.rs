//! The GPU's power in watts from AMD's Display Library (ADL), which the
//! Adrenalin driver installs as `atiadlxx.dll`. Loaded at runtime: without it
//! (another card's driver, no AMD driver) the watts stay unknown. Structures
//! and functions as in ADL SDK's `adl_sdk.h`, `adl_structures.h` and
//! `adl_defines.h` (GPUOpen display-library).

use std::{ffi::c_void, sync::OnceLock};

use windows_sys::{
    Win32::System::{
        LibraryLoader::{GetProcAddress, LoadLibraryW},
        Memory::{GetProcessHeap, HeapAlloc},
    },
    core::PCSTR,
};

use crate::hardware::{AdlAdapter, AdlSensor, PciAddress, adl_adapter_at, asic_power};

/// `ADL_MAX_PATH`.
const MAX_PATH: usize = 256;
/// `ADL_PMLOG_MAX_SENSORS`.
const MAX_SENSORS: usize = 256;
/// `ADL_OK`.
const OK: i32 = 0;

type Context = *mut c_void;
/// `ADL_MAIN_MALLOC_CALLBACK`.
type Malloc = unsafe extern "system" fn(i32) -> *mut c_void;
type ControlCreate = unsafe extern "C" fn(Malloc, i32, *mut Context) -> i32;
type NumberOfAdapters = unsafe extern "C" fn(Context, *mut i32) -> i32;
type AdapterInfoGet = unsafe extern "C" fn(Context, *mut AdapterInfo, i32) -> i32;
type QueryPmLog = unsafe extern "C" fn(Context, i32, *mut PmLogDataOutput) -> i32;
/// What `GetProcAddress` gives.
type Symbol = unsafe extern "system" fn() -> isize;

/// `AdapterInfo` with the fields Windows builds have.
#[repr(C)]
#[allow(dead_code, reason = "the layout ADL fills; only some fields are read")]
struct AdapterInfo {
    size: i32,
    adapter_index: i32,
    udid: [u8; MAX_PATH],
    bus_number: i32,
    device_number: i32,
    function_number: i32,
    vendor_id: i32,
    adapter_name: [u8; MAX_PATH],
    display_name: [u8; MAX_PATH],
    present: i32,
    exist: i32,
    driver_path: [u8; MAX_PATH],
    driver_path_ext: [u8; MAX_PATH],
    pnp_string: [u8; MAX_PATH],
    os_display_index: i32,
}

/// `ADLPMLogDataOutput`.
#[repr(C)]
struct PmLogDataOutput {
    size: i32,
    sensors: [AdlSensor; MAX_SENSORS],
}

struct Adl {
    context: Context,
    query: QueryPmLog,
    adapters: Vec<AdlAdapter>,
}

// SAFETY: the context is only used for PMLog queries, from the one sampler
// thread that runs at a time; ADL2 calls with their own context need no lock
// of the caller.
unsafe impl Send for Adl {}
// SAFETY: as above.
unsafe impl Sync for Adl {}

/// Loaded once per run of the PC service; a failed load stays failed.
static ADL: OnceLock<Option<Adl>> = OnceLock::new();

/// The power of the card at `at`, in watts; `None` without ADL, without an
/// ADL adapter there or without a reading.
pub fn power_watts(at: PciAddress) -> Option<f32> {
    let adl = ADL.get_or_init(load).as_ref()?;
    let index = adl_adapter_at(&adl.adapters, at)?;
    let mut out = PmLogDataOutput {
        size: size_of::<PmLogDataOutput>() as i32,
        sensors: [AdlSensor::default(); MAX_SENSORS],
    };
    // SAFETY: the context is open and `out` is the structure the call fills.
    if unsafe { (adl.query)(adl.context, index, &mut out) } != OK {
        return None;
    }
    asic_power(&out.sensors)
}

/// For ADL's own allocations; never freed, ADL asks for them rarely.
unsafe extern "system" fn malloc(size: i32) -> *mut c_void {
    // SAFETY: the process heap always exists.
    unsafe { HeapAlloc(GetProcessHeap(), 0, size.max(0) as usize) }
}

fn load() -> Option<Adl> {
    let name: Vec<u16> = "atiadlxx.dll\0".encode_utf16().collect();
    // SAFETY: a NUL-terminated name; the library stays loaded for the run.
    let lib = unsafe { LoadLibraryW(name.as_ptr()) };
    if lib.is_null() {
        return None;
    }
    let symbol = |name: &[u8]| {
        // SAFETY: `lib` is loaded and `name` is NUL-terminated.
        unsafe { GetProcAddress(lib, name.as_ptr() as PCSTR) }
    };
    // SAFETY: each symbol has the signature `adl_sdk`'s headers give it.
    let (create, count, info, query) = unsafe {
        (
            std::mem::transmute::<Symbol, ControlCreate>(symbol(b"ADL2_Main_Control_Create\0")?),
            std::mem::transmute::<Symbol, NumberOfAdapters>(symbol(
                b"ADL2_Adapter_NumberOfAdapters_Get\0",
            )?),
            std::mem::transmute::<Symbol, AdapterInfoGet>(symbol(
                b"ADL2_Adapter_AdapterInfo_Get\0",
            )?),
            std::mem::transmute::<Symbol, QueryPmLog>(symbol(b"ADL2_New_QueryPMLogData_Get\0")?),
        )
    };
    let mut context: Context = std::ptr::null_mut();
    // SAFETY: 1 lists only the adapters that are connected, as AMD's samples do.
    if unsafe { create(malloc, 1, &mut context) } != OK {
        return None;
    }
    let mut n = 0;
    // SAFETY: the context is open.
    if unsafe { count(context, &mut n) } != OK || n <= 0 {
        return None;
    }
    let mut list: Vec<AdapterInfo> = (0..n)
        .map(|_| {
            // SAFETY: AdapterInfo is plain data; all zero is valid.
            let mut a: AdapterInfo = unsafe { std::mem::zeroed() };
            a.size = size_of::<AdapterInfo>() as i32;
            a
        })
        .collect();
    let bytes = (size_of::<AdapterInfo>() * list.len()) as i32;
    // SAFETY: `list` holds `bytes` bytes of AdapterInfo.
    if unsafe { info(context, list.as_mut_ptr(), bytes) } != OK {
        return None;
    }
    let adapters = list
        .iter()
        .map(|a| AdlAdapter {
            index: a.adapter_index,
            at: PciAddress {
                bus: a.bus_number as u32,
                device: a.device_number as u32,
                function: a.function_number as u32,
            },
        })
        .collect();
    Some(Adl {
        context,
        query,
        adapters,
    })
}
