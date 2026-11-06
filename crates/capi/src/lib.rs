//! C ABI of coffee-core for native builds (spec §9.2). See `include/coffee_core.h`.
//!
//! Frames and semantics are the same as in WebAssembly. The instance is explicit, and host
//! callbacks are bound to it through `user_data`. Buffers carry their length in `ct_bytes`, so
//! there is no length prefix.

use coffee_abi::{HostTransport, Instance, ABI_VERSION};
use std::alloc::{alloc, dealloc, Layout};
use std::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy)]
pub struct CtBytes {
    pub ptr: *const u8,
    pub len: usize,
}

/// Host callback: receives a request frame, returns a response frame allocated with `ct_alloc`
/// (the core frees it), or `{NULL, 0}` when callbacks are not supported.
pub type CtHostFn = Option<unsafe extern "C" fn(user_data: *mut c_void, request: CtBytes) -> CtBytes>;

fn layout(len: usize) -> Option<Layout> {
    Layout::from_size_align(len.max(1), 1).ok()
}

#[no_mangle]
pub extern "C" fn ct_alloc(len: usize) -> *mut u8 {
    match layout(len) {
        // SAFETY: non-zero size.
        Some(l) => unsafe { alloc(l) },
        None => std::ptr::null_mut(),
    }
}

/// # Safety
/// `ptr` must come from `ct_alloc(len)` or be a `ct_call` result with its length.
#[no_mangle]
pub unsafe extern "C" fn ct_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    if let Some(l) = layout(len) {
        dealloc(ptr, l);
    }
}

#[no_mangle]
pub extern "C" fn ct_abi_version() -> u32 {
    ABI_VERSION
}

struct CHost {
    f: CtHostFn,
    user_data: *mut c_void,
}

impl HostTransport for CHost {
    fn call(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        let f = self.f?;
        // SAFETY: the callback contract is documented in coffee_core.h.
        unsafe {
            let r = f(self.user_data, CtBytes { ptr: request.as_ptr(), len: request.len() });
            if r.ptr.is_null() {
                return None;
            }
            let out = std::slice::from_raw_parts(r.ptr, r.len).to_vec();
            ct_free(r.ptr as *mut u8, r.len);
            Some(out)
        }
    }
}

/// Creates an instance. `host` may be NULL when no callbacks are used.
#[no_mangle]
pub extern "C" fn ct_instance_new(host: CtHostFn, user_data: *mut c_void) -> *mut c_void {
    Box::into_raw(Box::new(Instance::new(Box::new(CHost { f: host, user_data })))) as *mut c_void
}

/// # Safety
/// `instance` must come from `ct_instance_new` and must not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn ct_instance_free(instance: *mut c_void) {
    if !instance.is_null() {
        drop(Box::from_raw(instance as *mut Instance));
    }
}

/// Executes a request frame. The response must be freed with `ct_free(ptr, len)`.
///
/// # Safety
/// `instance` must be live and used by one thread at a time; `request` must be readable.
#[no_mangle]
pub unsafe extern "C" fn ct_call(instance: *mut c_void, request: CtBytes) -> CtBytes {
    if instance.is_null() {
        return CtBytes { ptr: std::ptr::null(), len: 0 };
    }
    let inst = &mut *(instance as *mut Instance);
    let req = if request.ptr.is_null() { &[][..] } else { std::slice::from_raw_parts(request.ptr, request.len) };
    let resp = inst.call(req);
    let ptr = ct_alloc(resp.len());
    if ptr.is_null() {
        return CtBytes { ptr: std::ptr::null(), len: 0 };
    }
    std::ptr::copy_nonoverlapping(resp.as_ptr(), ptr, resp.len());
    CtBytes { ptr, len: resp.len() }
}
