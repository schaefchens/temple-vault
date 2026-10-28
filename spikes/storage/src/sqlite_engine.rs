//! SQLite on the block layer through a small VFS: file 0 is the database, in
//! pages whose last 40 bytes are reserved for nonce and tag; file 1 is its
//! rollback journal, file 2 its write-ahead log.

use crate::blocks::Space;
use crate::workload::{Engine, Op, State};
use rusqlite::{Connection, OpenFlags, ffi, params};
use std::ffi::{CStr, c_char, c_int, c_void};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Once};

static SPACES: Mutex<Vec<(String, Arc<Mutex<Space>>)>> = Mutex::new(Vec::new());
static NAMES: AtomicUsize = AtomicUsize::new(0);
static REGISTER: Once = Once::new();
const VFS: &CStr = c"temple";

fn lookup(name: &str) -> Option<Arc<Mutex<Space>>> {
    SPACES.lock().unwrap().iter().find(|(n, _)| n == name).map(|(_, s)| s.clone())
}

/// Splits "space-3-journal" into ("space-3", journal file).
fn split(name: &str) -> (&str, usize) {
    if let Some(base) = name.strip_suffix("-journal") {
        (base, 1)
    } else if let Some(base) = name.strip_suffix("-wal") {
        (base, 2)
    } else {
        (name, 0)
    }
}

#[repr(C)]
struct File {
    base: ffi::sqlite3_file,
    space: *const Mutex<Space>,
    index: usize,
    /// An anonymous temp file, kept in memory and outside the vault.
    temp: *mut Vec<u8>,
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

unsafe fn space<'a>(file: *mut ffi::sqlite3_file) -> (&'a Mutex<Space>, usize) {
    let f = unsafe { &*(file as *mut File) };
    (unsafe { &*f.space }, f.index)
}

unsafe fn temp<'a>(file: *mut ffi::sqlite3_file) -> Option<&'a mut Vec<u8>> {
    let f = unsafe { &*(file as *mut File) };
    if f.temp.is_null() { None } else { Some(unsafe { &mut *f.temp }) }
}

unsafe extern "C" fn x_open(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    let f = unsafe { &mut *(file as *mut File) };
    f.base.pMethods = std::ptr::null();
    f.space = std::ptr::null();
    f.temp = std::ptr::null_mut();
    f.index = 0;
    if name.is_null() {
        f.temp = Box::into_raw(Box::new(Vec::new()));
    } else {
        let name = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
        if name.ends_with("-shm") {
            return ffi::SQLITE_CANTOPEN;
        }
        let (base, index) = split(&name);
        let Some(space) = lookup(base) else { return ffi::SQLITE_CANTOPEN };
        f.space = Arc::into_raw(space);
        f.index = index;
    }
    if !out_flags.is_null() {
        unsafe { *out_flags = flags };
    }
    f.base.pMethods = &IO;
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_close(file: *mut ffi::sqlite3_file) -> c_int {
    let f = unsafe { &mut *(file as *mut File) };
    if !f.temp.is_null() {
        drop(unsafe { Box::from_raw(f.temp) });
    }
    if !f.space.is_null() {
        drop(unsafe { Arc::from_raw(f.space) });
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_read(file: *mut ffi::sqlite3_file, buf: *mut c_void, amount: c_int, offset: i64) -> c_int {
    let out = unsafe { std::slice::from_raw_parts_mut(buf as *mut u8, amount as usize) };
    let n = match unsafe { temp(file) } {
        Some(t) => {
            let start = (offset as usize).min(t.len());
            let n = (t.len() - start).min(out.len());
            out[..n].copy_from_slice(&t[start..start + n]);
            n
        }
        None => {
            let (space, index) = unsafe { space(file) };
            match space.lock().unwrap().read(index, offset as u64, out) {
                Ok(n) => n,
                Err(_) => return ffi::SQLITE_IOERR_READ,
            }
        }
    };
    if n < out.len() {
        out[n..].fill(0);
        return ffi::SQLITE_IOERR_SHORT_READ;
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_write(file: *mut ffi::sqlite3_file, buf: *const c_void, amount: c_int, offset: i64) -> c_int {
    let data = unsafe { std::slice::from_raw_parts(buf as *const u8, amount as usize) };
    match unsafe { temp(file) } {
        Some(t) => {
            let end = offset as usize + data.len();
            if t.len() < end {
                t.resize(end, 0);
            }
            t[offset as usize..end].copy_from_slice(data);
            ffi::SQLITE_OK
        }
        None => {
            let (space, index) = unsafe { space(file) };
            match space.lock().unwrap().write(index, offset as u64, data) {
                Ok(()) => ffi::SQLITE_OK,
                Err(_) => ffi::SQLITE_IOERR_WRITE,
            }
        }
    }
}

unsafe extern "C" fn x_truncate(file: *mut ffi::sqlite3_file, size: i64) -> c_int {
    match unsafe { temp(file) } {
        Some(t) => {
            t.truncate(size as usize);
            ffi::SQLITE_OK
        }
        None => {
            let (space, index) = unsafe { space(file) };
            match space.lock().unwrap().set_len(index, size as u64) {
                Ok(()) => ffi::SQLITE_OK,
                Err(_) => ffi::SQLITE_IOERR_TRUNCATE,
            }
        }
    }
}

unsafe extern "C" fn x_sync(_file: *mut ffi::sqlite3_file, _flags: c_int) -> c_int {
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_file_size(file: *mut ffi::sqlite3_file, size: *mut i64) -> c_int {
    let n = match unsafe { temp(file) } {
        Some(t) => t.len() as u64,
        None => {
            let (space, index) = unsafe { space(file) };
            space.lock().unwrap().size(index)
        }
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

unsafe extern "C" fn x_file_control(_file: *mut ffi::sqlite3_file, _op: c_int, _arg: *mut c_void) -> c_int {
    ffi::SQLITE_NOTFOUND
}

unsafe extern "C" fn x_sector_size(_file: *mut ffi::sqlite3_file) -> c_int {
    4096
}

unsafe extern "C" fn x_device_characteristics(_file: *mut ffi::sqlite3_file) -> c_int {
    0
}

unsafe extern "C" fn x_delete(_vfs: *mut ffi::sqlite3_vfs, name: *const c_char, _sync_dir: c_int) -> c_int {
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
    let (base, index) = split(&name);
    if let Some(space) = lookup(base) {
        if space.lock().unwrap().set_len(index, 0).is_err() {
            return ffi::SQLITE_IOERR_DELETE;
        }
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_access(_vfs: *mut ffi::sqlite3_vfs, name: *const c_char, _flags: c_int, out: *mut c_int) -> c_int {
    let name = unsafe { CStr::from_ptr(name) }.to_string_lossy().into_owned();
    let (base, index) = split(&name);
    let exists = !name.ends_with("-shm") && lookup(base).is_some_and(|s| s.lock().unwrap().size(index) > 0);
    unsafe { *out = exists as c_int };
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_full_pathname(_vfs: *mut ffi::sqlite3_vfs, name: *const c_char, n: c_int, out: *mut c_char) -> c_int {
    let bytes = unsafe { CStr::from_ptr(name) }.to_bytes_with_nul();
    if bytes.len() > n as usize {
        return ffi::SQLITE_CANTOPEN;
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr() as *const c_char, out, bytes.len()) };
    ffi::SQLITE_OK
}

unsafe extern "C" fn x_randomness(_vfs: *mut ffi::sqlite3_vfs, n: c_int, out: *mut c_char) -> c_int {
    let out = unsafe { std::slice::from_raw_parts_mut(out as *mut u8, n as usize) };
    getrandom::fill(out).expect("system randomness");
    n
}

unsafe extern "C" fn x_sleep(_vfs: *mut ffi::sqlite3_vfs, micros: c_int) -> c_int {
    std::thread::sleep(std::time::Duration::from_micros(micros as u64));
    micros
}

unsafe extern "C" fn x_current_time(_vfs: *mut ffi::sqlite3_vfs, out: *mut f64) -> c_int {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs_f64();
    unsafe { *out = 2440587.5 + secs / 86400.0 };
    ffi::SQLITE_OK
}

fn register() {
    REGISTER.call_once(|| {
        let vfs = Box::leak(Box::new(ffi::sqlite3_vfs {
            iVersion: 1,
            szOsFile: std::mem::size_of::<File>() as c_int,
            mxPathname: 512,
            pNext: std::ptr::null_mut(),
            zName: VFS.as_ptr(),
            pAppData: std::ptr::null_mut(),
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
        assert_eq!(unsafe { ffi::sqlite3_vfs_register(vfs, 0) }, ffi::SQLITE_OK);
    });
}

pub struct SqliteEngine {
    conn: Connection,
    name: String,
}

impl Drop for SqliteEngine {
    fn drop(&mut self) {
        // Close first: closing checkpoints and deletes the write-ahead log through the registry.
        let placeholder = Connection::open_in_memory().expect("in-memory database");
        drop(std::mem::replace(&mut self.conn, placeholder));
        SPACES.lock().unwrap().retain(|(n, _)| n != &self.name);
    }
}

fn text(e: impl std::fmt::Display) -> String {
    e.to_string()
}

impl SqliteEngine {
    /// `wal`: write-ahead log with exclusive locking, so SQLite needs no shared-memory file;
    /// `autocheckpoint`: pages between checkpoints – `None` for SQLite's default of 1000, 0 for none.
    pub fn open(space: Arc<Mutex<Space>>, secure_delete: bool, wal: bool, autocheckpoint: Option<u32>) -> Result<SqliteEngine, String> {
        register();
        let fresh = space.lock().unwrap().size(0) == 0;
        let name = format!("space-{}", NAMES.fetch_add(1, Ordering::SeqCst));
        SPACES.lock().unwrap().push((name.clone(), space));
        let flags = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE;
        let conn = match Connection::open_with_flags_and_vfs(&name, flags, VFS) {
            Ok(conn) => conn,
            Err(e) => {
                SPACES.lock().unwrap().retain(|(n, _)| n != &name);
                return Err(text(e));
            }
        };
        // From here on, dropping the engine closes and unregisters, also on errors.
        let engine = SqliteEngine { conn, name };
        let conn = &engine.conn;
        if fresh {
            // Leave the last 40 bytes of every page to the block layer.
            let mut reserve: c_int = 40;
            let rc = unsafe {
                ffi::sqlite3_file_control(
                    conn.handle(),
                    c"main".as_ptr(),
                    ffi::SQLITE_FCNTL_RESERVE_BYTES,
                    &mut reserve as *mut c_int as *mut c_void,
                )
            };
            if rc != ffi::SQLITE_OK {
                return Err(format!("reserve bytes: {rc}"));
            }
        }
        let wanted = if wal { "wal" } else { "truncate" };
        if wal {
            conn.execute_batch("PRAGMA locking_mode=EXCLUSIVE").map_err(text)?;
        }
        let mode: String = conn.query_row(&format!("PRAGMA journal_mode={wanted}"), [], |r| r.get(0)).map_err(text)?;
        if mode != wanted {
            return Err(format!("journal mode {mode}"));
        }
        conn.execute_batch(&format!(
            "PRAGMA synchronous=FULL; PRAGMA temp_store=MEMORY; PRAGMA secure_delete={};
             CREATE TABLE IF NOT EXISTS kv(k INTEGER PRIMARY KEY, v TEXT NOT NULL);",
            if secure_delete { "ON" } else { "OFF" }
        ))
        .map_err(text)?;
        if let Some(pages) = autocheckpoint {
            conn.execute_batch(&format!("PRAGMA wal_autocheckpoint={pages}")).map_err(text)?;
        }
        Ok(engine)
    }
}

impl SqliteEngine {
    /// Copies the log into the database and truncates it, as when locking a space.
    pub fn checkpoint(&self) -> Result<(), String> {
        self.conn.query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |_| Ok(())).map_err(text)
    }

    /// Copies the log into the database and leaves the log as it is.
    pub fn checkpoint_full(&self) -> Result<(), String> {
        self.conn.query_row("PRAGMA wal_checkpoint(FULL)", [], |_| Ok(())).map_err(text)
    }
}

impl Engine for SqliteEngine {
    fn commit(&mut self, ops: &[Op]) -> Result<(), String> {
        let tx = self.conn.transaction().map_err(text)?;
        for op in ops {
            match op {
                Op::Put(key, value) => tx
                    .execute(
                        "INSERT INTO kv(k, v) VALUES(?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
                        params![key, value],
                    )
                    .map_err(text)?,
                Op::Del(key) => tx.execute("DELETE FROM kv WHERE k = ?1", params![key]).map_err(text)?,
            };
        }
        tx.commit().map_err(text)
    }

    fn read_all(&mut self) -> Result<State, String> {
        let mut stmt = self.conn.prepare("SELECT k, v FROM kv ORDER BY k").map_err(text)?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, u32>(0)?, r.get::<_, String>(1)?))).map_err(text)?;
        rows.collect::<Result<State, _>>().map_err(text)
    }

    fn check(&mut self) -> Result<bool, String> {
        let result: String = self.conn.query_row("PRAGMA integrity_check", [], |r| r.get(0)).map_err(text)?;
        Ok(result == "ok")
    }
}
