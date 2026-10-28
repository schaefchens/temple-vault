//! SQLite's heap, wiped on every free: pages and rows SQLite decrypted and cached don't
//! outlive the space that held them.

use rusqlite::ffi;
use std::alloc::{Layout, alloc, dealloc};
use std::ffi::{c_int, c_void};
use std::ptr;
use zeroize::Zeroize;

/// Each allocation starts with its size; 16 bytes keep SQLite's 8-byte alignment.
const HEADER: usize = 16;

/// Installs the heap. Only possible before SQLite's first use anywhere in the process.
pub(super) fn install() -> bool {
    let methods = ffi::sqlite3_mem_methods {
        xMalloc: Some(x_malloc),
        xFree: Some(x_free),
        xRealloc: Some(x_realloc),
        xSize: Some(x_size),
        xRoundup: Some(x_roundup),
        xInit: Some(x_init),
        xShutdown: Some(x_shutdown),
        pAppData: ptr::null_mut(),
    };
    // SQLite copies the methods.
    unsafe {
        ffi::sqlite3_config(ffi::SQLITE_CONFIG_MALLOC, &methods as *const ffi::sqlite3_mem_methods) == ffi::SQLITE_OK
    }
}

fn layout(size: usize) -> Option<Layout> {
    Layout::from_size_align(HEADER.checked_add(size)?, HEADER).ok()
}

unsafe extern "C" fn x_malloc(n: c_int) -> *mut c_void {
    let Some(layout) = usize::try_from(n).ok().and_then(layout) else { return ptr::null_mut() };
    let base = unsafe { alloc(layout) };
    if base.is_null() {
        return ptr::null_mut();
    }
    unsafe {
        (base as *mut usize).write(layout.size() - HEADER);
        base.add(HEADER) as *mut c_void
    }
}

unsafe fn size_of(p: *mut c_void) -> usize {
    unsafe { ((p as *mut u8).sub(HEADER) as *const usize).read() }
}

unsafe extern "C" fn x_free(p: *mut c_void) {
    if p.is_null() {
        return;
    }
    unsafe {
        let size = size_of(p);
        let base = (p as *mut u8).sub(HEADER);
        std::slice::from_raw_parts_mut(base, HEADER + size).zeroize();
        dealloc(base, Layout::from_size_align_unchecked(HEADER + size, HEADER));
    }
}

unsafe extern "C" fn x_realloc(p: *mut c_void, n: c_int) -> *mut c_void {
    if p.is_null() {
        return unsafe { x_malloc(n) };
    }
    let new = unsafe { x_malloc(n) };
    if !new.is_null() {
        unsafe {
            let keep = size_of(p).min(n as usize);
            ptr::copy_nonoverlapping(p as *const u8, new as *mut u8, keep);
            x_free(p);
        }
    }
    new
}

unsafe extern "C" fn x_size(p: *mut c_void) -> c_int {
    if p.is_null() { 0 } else { unsafe { size_of(p) as c_int } }
}

unsafe extern "C" fn x_roundup(n: c_int) -> c_int {
    n.saturating_add(7) & !7
}

unsafe extern "C" fn x_init(_: *mut c_void) -> c_int {
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_shutdown(_: *mut c_void) {}
