//! SQLite on a space's blocks: the VFS "temple". The database file and its write-ahead log
//! live in the space's [`Store`]; a rollback journal (only used while a space is set up)
//! and temporary files stay in memory and are wiped when closed. With exclusive locking,
//! the log's index stays in SQLite's heap, so there's no shared-memory file.

use super::heap;
use super::store::{File, PAYLOAD, Store};
use crate::{BLOCK, Error};
use rusqlite::ffi;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::io;
use std::ptr;
use std::slice;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};
use zeroize::Zeroizing;

pub(crate) const NAME: &CStr = c"temple";

/// Pragmas that would break a space's guarantees – crash and copy safety, deleted data
/// overwritten, the checkpoint record fitting the superblock. Only Temple may set them.
const GUARDED: &[&str] = &[
    "journal_mode",
    "locking_mode",
    "synchronous",
    "wal_autocheckpoint",
    "wal_checkpoint",
    "journal_size_limit",
    "secure_delete",
    "page_size",
    "max_page_count",
    "mmap_size",
    "temp_store",
    "temp_store_directory",
    "data_store_directory",
    "writable_schema",
];

/// The open spaces' stores, by the name their database was opened under.
static STORES: Mutex<Vec<(String, Arc<Mutex<Store>>)>> = Mutex::new(Vec::new());

/// Installs SQLite's wiping heap and registers the VFS, once per process.
pub(crate) fn install() -> Result<(), Error> {
    static INSTALLED: OnceLock<bool> = OnceLock::new();
    if *INSTALLED.get_or_init(|| heap::install() && register()) {
        Ok(())
    } else {
        Err(io::Error::other(
            "SQLite was in use before Temple: Temple must open a vault before anything else uses SQLite",
        )
        .into())
    }
}

/// Makes a store available to SQLite under a fresh random name, which the app's SQL can't
/// guess.
pub(crate) fn add(store: Arc<Mutex<Store>>) -> String {
    let mut stores = STORES.lock().unwrap();
    loop {
        let mut bytes = [0u8; 8];
        crate::crypto::random(&mut bytes);
        let name = format!("space-{}", bytes.iter().map(|b| format!("{b:02x}")).collect::<String>());
        if stores.iter().all(|(n, _)| *n != name) {
            stores.push((name.clone(), store));
            return name;
        }
    }
}

pub(crate) fn remove(name: &str) {
    STORES.lock().unwrap().retain(|(n, _)| n != name);
}

fn find(name: &str) -> Option<Arc<Mutex<Store>>> {
    STORES.lock().unwrap().iter().find(|(n, _)| n == name).map(|(_, store)| store.clone())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Database,
    Log,
    Memory,
}

/// Splits "space-3-wal" into "space-3" and what the suffix names.
fn split(name: &str) -> (&str, Kind) {
    if let Some(base) = name.strip_suffix("-wal") {
        (base, Kind::Log)
    } else if let Some(base) = name.strip_suffix("-journal") {
        (base, Kind::Memory)
    } else {
        (name, Kind::Database)
    }
}

#[repr(C)]
struct Handle {
    base: ffi::sqlite3_file,
    kind: Kind,
    store: *const Mutex<Store>,
    memory: *mut Zeroizing<Vec<u8>>,
}

impl Handle {
    fn store(&self) -> Option<MutexGuard<'_, Store>> {
        unsafe { &*self.store }.lock().ok()
    }

    fn file(&self) -> File {
        if self.kind == Kind::Log { File::Log } else { File::Database }
    }

    fn memory(&mut self) -> &mut Zeroizing<Vec<u8>> {
        unsafe { &mut *self.memory }
    }
}

unsafe fn handle<'a>(file: *mut ffi::sqlite3_file) -> &'a mut Handle {
    unsafe { &mut *(file as *mut Handle) }
}

static IO: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(x_close),
    xRead: Some(x_read),
    xWrite: Some(x_write),
    xTruncate: Some(x_truncate),
    xSync: Some(x_sync),
    xFileSize: Some(x_file_size),
    xLock: Some(x_lock),
    xUnlock: Some(x_lock),
    xCheckReservedLock: Some(x_check_reserved_lock),
    xFileControl: Some(x_file_control),
    xSectorSize: Some(x_sector_size),
    xDeviceCharacteristics: Some(x_device_characteristics),
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};

unsafe extern "C" fn x_open(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    unsafe { (*file).pMethods = ptr::null() };
    let (kind, store) = if name.is_null() {
        (Kind::Memory, ptr::null())
    } else {
        let Ok(name) = unsafe { CStr::from_ptr(name) }.to_str() else { return ffi::SQLITE_CANTOPEN };
        let (base, kind) = split(name);
        let Some(store) = find(base) else { return ffi::SQLITE_CANTOPEN };
        if kind == Kind::Database {
            // One connection at a time: locking is left out, so a second one would corrupt.
            let Ok(mut store) = store.lock() else { return ffi::SQLITE_CANTOPEN };
            if store.database_open {
                return ffi::SQLITE_CANTOPEN;
            }
            store.database_open = true;
        }
        (kind, if kind == Kind::Memory { ptr::null() } else { Arc::into_raw(store) })
    };
    let memory =
        if kind == Kind::Memory { Box::into_raw(Box::new(Zeroizing::new(Vec::new()))) } else { ptr::null_mut() };
    unsafe { (file as *mut Handle).write(Handle { base: ffi::sqlite3_file { pMethods: &IO }, kind, store, memory }) };
    if !out_flags.is_null() {
        unsafe { *out_flags = flags };
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_close(file: *mut ffi::sqlite3_file) -> c_int {
    let h = unsafe { handle(file) };
    if !h.memory.is_null() {
        drop(unsafe { Box::from_raw(h.memory) });
    }
    if !h.store.is_null() {
        if h.kind == Kind::Database
            && let Some(mut store) = h.store()
        {
            store.database_open = false;
        }
        drop(unsafe { Arc::from_raw(h.store) });
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_read(file: *mut ffi::sqlite3_file, buf: *mut c_void, amount: c_int, offset: i64) -> c_int {
    let h = unsafe { handle(file) };
    let out = unsafe { slice::from_raw_parts_mut(buf as *mut u8, amount as usize) };
    let read = match h.kind {
        Kind::Memory => {
            let memory = h.memory();
            let start = (offset as usize).min(memory.len());
            let n = (memory.len() - start).min(out.len());
            out[..n].copy_from_slice(&memory[start..start + n]);
            Ok(n)
        }
        _ => match h.store() {
            Some(store) => store.read(h.file(), offset as u64, out),
            None => Err(Error::Damaged),
        },
    };
    match read {
        Ok(n) if n == out.len() => ffi::SQLITE_OK,
        Ok(n) => {
            out[n..].fill(0);
            ffi::SQLITE_IOERR_SHORT_READ
        }
        Err(_) => ffi::SQLITE_IOERR_READ,
    }
}

unsafe extern "C" fn x_write(file: *mut ffi::sqlite3_file, buf: *const c_void, amount: c_int, offset: i64) -> c_int {
    let h = unsafe { handle(file) };
    let data = unsafe { slice::from_raw_parts(buf as *const u8, amount as usize) };
    let written = match h.kind {
        Kind::Memory => {
            let memory = h.memory();
            let end = offset as usize + data.len();
            if memory.len() < end {
                // Growing in place, so no unwiped copy is left behind.
                let mut grown = Zeroizing::new(Vec::with_capacity(end.max(2 * memory.capacity())));
                grown.extend_from_slice(memory);
                grown.resize(end, 0);
                *memory = grown;
            }
            memory[offset as usize..end].copy_from_slice(data);
            Ok(())
        }
        _ => match h.store() {
            Some(mut store) => store.write(h.file(), offset as u64, data),
            None => Err(Error::Damaged),
        },
    };
    if written.is_ok() { ffi::SQLITE_OK } else { ffi::SQLITE_IOERR_WRITE }
}

unsafe extern "C" fn x_truncate(file: *mut ffi::sqlite3_file, size: i64) -> c_int {
    let h = unsafe { handle(file) };
    let truncated = match h.kind {
        Kind::Memory => {
            h.memory().truncate(size as usize);
            Ok(())
        }
        _ => match h.store() {
            Some(mut store) => store.truncate(h.file(), size as u64),
            None => Err(Error::Damaged),
        },
    };
    if truncated.is_ok() { ffi::SQLITE_OK } else { ffi::SQLITE_IOERR_TRUNCATE }
}

unsafe extern "C" fn x_sync(file: *mut ffi::sqlite3_file, _flags: c_int) -> c_int {
    let h = unsafe { handle(file) };
    let synced = match h.kind {
        Kind::Memory => Ok(()),
        _ => match h.store() {
            Some(mut store) => store.sync(),
            None => Err(Error::Damaged),
        },
    };
    if synced.is_ok() { ffi::SQLITE_OK } else { ffi::SQLITE_IOERR_FSYNC }
}

unsafe extern "C" fn x_file_size(file: *mut ffi::sqlite3_file, size: *mut i64) -> c_int {
    let h = unsafe { handle(file) };
    let n = match h.kind {
        Kind::Memory => h.memory().len() as u64,
        _ => match h.store() {
            Some(store) => store.size(h.file()),
            None => return ffi::SQLITE_IOERR_FSTAT,
        },
    };
    unsafe { *size = n as i64 };
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_lock(_file: *mut ffi::sqlite3_file, _level: c_int) -> c_int {
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_check_reserved_lock(_file: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
    unsafe { *out = 0 };
    ffi::SQLITE_OK
}

/// Refuses guarded pragmas unless Temple runs them; SQLite handles everything else.
unsafe extern "C" fn x_file_control(file: *mut ffi::sqlite3_file, op: c_int, arg: *mut c_void) -> c_int {
    let h = unsafe { handle(file) };
    if op != ffi::SQLITE_FCNTL_PRAGMA || h.kind != Kind::Database {
        return ffi::SQLITE_NOTFOUND;
    }
    let args = arg as *mut *mut c_char;
    let (name, value) = unsafe { (CStr::from_ptr(*args.add(1)), *args.add(2)) };
    let name = name.to_string_lossy().to_ascii_lowercase();
    let guarded = GUARDED.contains(&name.as_str()) && (!value.is_null() || name == "wal_checkpoint");
    if !guarded || h.store().is_some_and(|store| store.admin) {
        return ffi::SQLITE_NOTFOUND;
    }
    let message = format!("PRAGMA {name} is Temple's to set\0");
    unsafe {
        let copy = ffi::sqlite3_malloc(message.len() as c_int) as *mut u8;
        if !copy.is_null() {
            ptr::copy_nonoverlapping(message.as_ptr(), copy, message.len());
        }
        *args = copy as *mut c_char;
    }
    ffi::SQLITE_ERROR
}

/// The log's sectors are its blocks: SQLite then pads every commit to a block boundary, so
/// a commit never rewrites a block that an earlier one made durable.
unsafe extern "C" fn x_sector_size(file: *mut ffi::sqlite3_file) -> c_int {
    match unsafe { handle(file) }.kind {
        Kind::Log => PAYLOAD as c_int,
        _ => BLOCK as c_int,
    }
}

/// No promises: in particular not SQLITE_IOCAP_POWERSAFE_OVERWRITE, which would turn the
/// log's padding off.
unsafe extern "C" fn x_device_characteristics(_file: *mut ffi::sqlite3_file) -> c_int {
    0
}

unsafe extern "C" fn x_delete(_vfs: *mut ffi::sqlite3_vfs, name: *const c_char, _sync_dir: c_int) -> c_int {
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    let (base, kind) = split(&name);
    if kind != Kind::Log {
        return ffi::SQLITE_OK;
    }
    let deleted = match find(base) {
        Some(store) => store.lock().map_err(|_| Error::Damaged).and_then(|mut store| store.truncate(File::Log, 0)),
        None => Ok(()),
    };
    if deleted.is_ok() { ffi::SQLITE_OK } else { ffi::SQLITE_IOERR_DELETE }
}

unsafe extern "C" fn x_access(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    _flags: c_int,
    out: *mut c_int,
) -> c_int {
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy();
    let (base, kind) = split(&name);
    let exists = match (kind, find(base)) {
        (Kind::Database, Some(_)) => true,
        (Kind::Log, Some(store)) => store.lock().is_ok_and(|store| store.size(File::Log) > 0),
        _ => false,
    };
    unsafe { *out = exists as c_int };
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_full_pathname(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    n: c_int,
    out: *mut c_char,
) -> c_int {
    let bytes = unsafe { CStr::from_ptr(name) }.to_bytes_with_nul();
    if bytes.len() > n as usize {
        return ffi::SQLITE_CANTOPEN;
    }
    unsafe { ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, out, bytes.len()) };
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_randomness(_vfs: *mut ffi::sqlite3_vfs, n: c_int, out: *mut c_char) -> c_int {
    crate::crypto::random(unsafe { slice::from_raw_parts_mut(out as *mut u8, n as usize) });
    n
}

unsafe extern "C" fn x_sleep(_vfs: *mut ffi::sqlite3_vfs, micros: c_int) -> c_int {
    std::thread::sleep(std::time::Duration::from_micros(micros as u64));
    micros
}

unsafe extern "C" fn x_current_time(_vfs: *mut ffi::sqlite3_vfs, out: *mut f64) -> c_int {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default();
    // A Julian day number.
    unsafe { *out = 2440587.5 + now.as_secs_f64() / 86400.0 };
    ffi::SQLITE_OK
}

fn register() -> bool {
    let vfs = Box::leak(Box::new(ffi::sqlite3_vfs {
        iVersion: 1,
        szOsFile: std::mem::size_of::<Handle>() as c_int,
        mxPathname: 512,
        pNext: ptr::null_mut(),
        zName: NAME.as_ptr(),
        pAppData: ptr::null_mut(),
        xOpen: Some(x_open),
        xDelete: Some(x_delete),
        xAccess: Some(x_access),
        xFullPathname: Some(x_full_pathname),
        xDlOpen: None,
        xDlError: None,
        xDlSym: None,
        xDlClose: None,
        xRandomness: Some(x_randomness),
        xSleep: Some(x_sleep),
        xCurrentTime: Some(x_current_time),
        xGetLastError: None,
        xCurrentTimeInt64: None,
        xSetSystemCall: None,
        xGetSystemCall: None,
        xNextSystemCall: None,
    }));
    unsafe { ffi::sqlite3_vfs_register(vfs, 0) == ffi::SQLITE_OK }
}
