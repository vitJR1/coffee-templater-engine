//! `coffee_core.wasm`: WebAssembly ABI of coffee-core (spec §9.1).
//!
//! Exports: `memory`, `ct_abi_version`, `ct_alloc`, `ct_free`, `ct_call`.
//! Import: `ct_host.call(req_ptr, req_len) -> u32` for host callbacks.
//!
//! Buffers returned by the core and by the host are laid out as `[u32 frame_len LE][frame]` and
//! are freed with `ct_free(ptr, frame_len + 4)`. A host answering a callback must therefore
//! allocate exactly `frame_len + 4` bytes with `ct_alloc`.

use coffee_abi::{error_response, HostTransport, Instance, ABI_VERSION};
use coffee_core::ErrorCode;
use std::alloc::{alloc, dealloc, Layout};
use std::cell::Cell;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "ct_host")]
extern "C" {
    #[link_name = "call"]
    fn host_call(req_ptr: u32, req_len: u32) -> u32;
}

/// Native builds of this crate (e.g. `cargo test --workspace`) have no host import.
#[cfg(not(target_arch = "wasm32"))]
unsafe fn host_call(_req_ptr: u32, _req_len: u32) -> u32 {
    0
}

fn layout(len: usize) -> Option<Layout> {
    Layout::from_size_align(len.max(1), 1).ok()
}

/// Allocates `len` bytes in module memory. Returns 0 on failure.
#[no_mangle]
pub extern "C" fn ct_alloc(len: u32) -> u32 {
    match layout(len as usize) {
        // SAFETY: the layout has a non-zero size.
        Some(l) => unsafe { alloc(l) as usize as u32 },
        None => 0,
    }
}

/// Frees memory obtained from `ct_alloc` or returned by `ct_call`.
///
/// # Safety
/// `ptr` must come from `ct_alloc(len)` (or be a `ct_call` result freed with `frame_len + 4`).
#[no_mangle]
pub unsafe extern "C" fn ct_free(ptr: u32, len: u32) {
    if ptr == 0 {
        return;
    }
    if let Some(l) = layout(len as usize) {
        dealloc(ptr as usize as *mut u8, l);
    }
}

#[no_mangle]
pub extern "C" fn ct_abi_version() -> u32 {
    ABI_VERSION
}

/// Copies `frame` into a fresh `[u32 len][frame]` buffer.
fn to_prefixed(frame: &[u8]) -> u32 {
    let total = frame.len() + 4;
    let ptr = ct_alloc(total as u32);
    if ptr == 0 {
        return 0;
    }
    // SAFETY: `ptr` points to `total` freshly allocated bytes.
    unsafe {
        let dst = std::slice::from_raw_parts_mut(ptr as usize as *mut u8, total);
        dst[..4].copy_from_slice(&(frame.len() as u32).to_le_bytes());
        dst[4..].copy_from_slice(frame);
    }
    ptr
}

struct WasmHost;

impl HostTransport for WasmHost {
    fn call(&mut self, request: &[u8]) -> Option<Vec<u8>> {
        if STATE.get() == POISONED {
            return None;
        }
        // SAFETY: the request slice lives in module memory for the duration of the call.
        let ptr = unsafe { host_call(request.as_ptr() as usize as u32, request.len() as u32) };
        if STATE.get() == POISONED {
            // ct_call ran from inside the callback and poisoned the instance; stop this call too.
            return None;
        }
        if ptr == 0 {
            return None;
        }
        // SAFETY: the host wrote `[u32 len][frame]` at `ptr` into a buffer from `ct_alloc`.
        unsafe {
            let head = std::slice::from_raw_parts(ptr as usize as *const u8, 4);
            let len = u32::from_le_bytes([head[0], head[1], head[2], head[3]]) as usize;
            let frame = std::slice::from_raw_parts((ptr as usize + 4) as *const u8, len).to_vec();
            ct_free(ptr, (len + 4) as u32);
            Some(frame)
        }
    }
}

// The module is single-threaded. A trap (a panic, or a host exception thrown from a callback)
// stops a call half way and leaves the shadow stack and the heap of that call behind: neither
// can be restored from inside the module. The state flag notices the trap on the next call and
// poisons the instance for good; every later call answers `internal` and the host must create a
// new WebAssembly instance. The state stays BUSY during host callbacks: a `ct_call` made from
// inside a callback cannot be told apart from a trapped callback, so it poisons the instance too.
const IDLE: u8 = 0;
const BUSY: u8 = 1;
const POISONED: u8 = 2;

const POISONED_MESSAGE: &str = "The WebAssembly instance is unusable after a trap (a panic, a host exception thrown from a callback, \
or ct_call made from inside a callback). Create a new instance.";

thread_local! {
    static STATE: Cell<u8> = const { Cell::new(IDLE) };
    static INSTANCE: Cell<Option<Box<Instance>>> = const { Cell::new(None) };
}

fn poisoned() -> u32 {
    STATE.set(POISONED);
    to_prefixed(&error_response(ErrorCode::Internal, POISONED_MESSAGE))
}

/// Executes a request frame; returns a pointer to `[u32 frame_len][response frame]`.
///
/// # Safety
/// `req_ptr..req_ptr+req_len` must be readable module memory (allocated with `ct_alloc`).
#[no_mangle]
pub unsafe extern "C" fn ct_call(req_ptr: u32, req_len: u32) -> u32 {
    if STATE.get() != IDLE {
        // BUSY: the previous call trapped, or this is a re-entrant call from a host callback.
        return poisoned();
    }
    STATE.set(BUSY);
    let request = if req_len == 0 { &[][..] } else { std::slice::from_raw_parts(req_ptr as usize as *const u8, req_len as usize) };
    let mut instance = INSTANCE.take().unwrap_or_else(|| Box::new(Instance::new(Box::new(WasmHost))));
    let response = instance.call(request);
    if STATE.get() == POISONED {
        // a re-entrant call happened during this one: drop the result, keep the instance dead
        std::mem::forget(instance);
        return poisoned();
    }
    INSTANCE.set(Some(instance));
    STATE.set(IDLE);
    to_prefixed(&response)
}
