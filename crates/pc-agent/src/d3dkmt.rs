//! The graphics adapters as Windows' kernel graphics interface lists them,
//! for the hardware readings and the machine facts.

use windows_sys::Wdk::Graphics::Direct3D::{
    D3DKMT_ADAPTERINFO, D3DKMT_CLOSEADAPTER, D3DKMT_ENUMADAPTERS2, D3DKMT_QUERYADAPTERINFO,
    D3DKMTCloseAdapter, D3DKMTEnumAdapters2, D3DKMTQueryAdapterInfo, KMTQUERYADAPTERINFOTYPE,
};

/// `f` over every adapter, whose handles stay open until it returns;
/// `None` if Windows lists none.
pub fn with_adapters<R>(f: impl FnOnce(&[D3DKMT_ADAPTERINFO]) -> R) -> Option<R> {
    let mut list = D3DKMT_ENUMADAPTERS2 {
        NumAdapters: 0,
        pAdapters: std::ptr::null_mut(),
    };
    // SAFETY: a null list asks for the count only.
    if unsafe { D3DKMTEnumAdapters2(&mut list) } != 0 {
        return None;
    }
    let mut adapters = vec![D3DKMT_ADAPTERINFO::default(); list.NumAdapters as usize];
    list.pAdapters = adapters.as_mut_ptr();
    // SAFETY: `adapters` holds NumAdapters entries.
    if unsafe { D3DKMTEnumAdapters2(&mut list) } != 0 {
        return None;
    }
    adapters.truncate(list.NumAdapters as usize);
    let result = f(&adapters);
    for info in &adapters {
        let close = D3DKMT_CLOSEADAPTER {
            hAdapter: info.hAdapter,
        };
        // SAFETY: each handle from the enumeration is closed once.
        unsafe { D3DKMTCloseAdapter(&close) };
    }
    Some(result)
}

/// Asks the adapter for `kind`, written into `out`.
///
/// # Safety
/// `handle` is an open adapter and `T` the structure `kind` fills.
pub unsafe fn query<T>(handle: u32, kind: KMTQUERYADAPTERINFOTYPE, out: &mut T) -> bool {
    let mut q = D3DKMT_QUERYADAPTERINFO {
        hAdapter: handle,
        Type: kind,
        pPrivateDriverData: (out as *mut T).cast(),
        PrivateDriverDataSize: size_of::<T>() as u32,
    };
    // SAFETY: `out` is valid for `size_of::<T>()` bytes; the rest is the caller's.
    unsafe { D3DKMTQueryAdapterInfo(&mut q) == 0 }
}
