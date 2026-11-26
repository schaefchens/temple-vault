//! The app's native code reading a space's objects – a media player reading audio piece by
//! piece on a thread of its own, so that the audio never passes through JavaScript – while
//! JavaScript has the space open: through a handle, never a second opening. Each read waits its
//! turn on Temple's thread, among JavaScript's calls. `TempleReader` in Swift and in Java, for
//! Kotlin, wraps the functions here; bindings/react-native ships both.

use crate::{Space, TempleError, background, locked};
use std::ffi::{CStr, c_char};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use zeroize::{Zeroize, Zeroizing};

/// The handles given out, each with the space it reads – weakly: a handle never keeps a space
/// open.
static HANDLES: Mutex<Vec<(String, Weak<Space>)>> = Mutex::new(Vec::new());

fn handles() -> MutexGuard<'static, Vec<(String, Weak<Space>)>> {
    HANDLES.lock().unwrap_or_else(PoisonError::into_inner)
}

#[uniffi::export]
impl Space {
    /// A handle for the app's native code to read this space's objects with, from any thread,
    /// while the space is open. Reads with it fail as `Locked` once the space locks or
    /// [`release_native_reader`] gives the handle up.
    pub async fn native_reader(self: Arc<Self>) -> Result<String, TempleError> {
        let space = Arc::downgrade(&self);
        self.with(move |_| {
            let mut bytes = [0u8; 16];
            getrandom::fill(&mut bytes).map_err(|e| TempleError::Io(e.to_string()))?;
            let handle: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
            let mut handles = handles();
            handles.retain(|(_, space)| space.strong_count() > 0);
            handles.push((handle.clone(), space));
            Ok(handle)
        })
        .await
    }
}

/// Gives up a handle from [`Space::native_reader`]: reads with it fail from now on.
#[uniffi::export]
pub fn release_native_reader(handle: String) {
    handles().retain(|(known, _)| *known != handle);
}

/// A space locks: its handles go with it.
pub(crate) fn forget(space: &Space) {
    handles().retain(|(_, known)| !std::ptr::eq(known.as_ptr(), space));
}

/// An object's size in bytes, read through a handle.
pub fn object_len(handle: &str, id: i64) -> Result<u64, TempleError> {
    reading(handle, move |space| Ok(space.object_len(id)?))
}

/// Bytes `start..end` of an object, read through a handle – up to its end, if that comes first.
/// Wiped as they drop.
pub fn read_object(handle: &str, id: i64, start: u64, end: u64) -> Result<Zeroizing<Vec<u8>>, TempleError> {
    reading(handle, move |space| Ok(Zeroizing::new(space.read_object(id, start..end)?)))
}

/// Runs `work` on the space a handle reads, in its turn on Temple's thread, and waits for it.
fn reading<T: Send + 'static>(
    handle: &str,
    work: impl FnOnce(&temple_vault::Space) -> Result<T, TempleError> + Send + 'static,
) -> Result<T, TempleError> {
    let handle = handle.to_owned();
    background::run_blocking(move || {
        // Looked up in its turn: a lock or a release that came first counts.
        let space = handles().iter().find(|(known, _)| *known == handle).and_then(|(_, space)| space.upgrade());
        let space = space.ok_or_else(given_up)?;
        let open = space.guard();
        // Stopped by a lock: by the time native code hears of it, the space has locked.
        work(open.as_ref().ok_or_else(locked)?).map_err(|e| match e {
            TempleError::Interrupted(_) => locked(),
            e => e,
        })
    })
}

fn given_up() -> TempleError {
    TempleError::Locked("no open space for this handle: it was given up, or its space has locked".into())
}

/// The kinds of error native code tells apart, by the names TempleError gives them in
/// JavaScript; `TempleReader` in Swift and Java numbers them alike.
const LOCKED: i32 = 1;
const NO_SUCH_OBJECT: i32 = 2;
const DAMAGED: i32 = 3;
const DATABASE: i32 = 4;
const IO: i32 = 5;

fn kind(error: &TempleError) -> i32 {
    match error {
        TempleError::Locked(_) | TempleError::Interrupted(_) => LOCKED,
        TempleError::NoSuchObject(_) => NO_SUCH_OBJECT,
        TempleError::Damaged(_) => DAMAGED,
        TempleError::Database(_) => DATABASE,
        _ => IO,
    }
}

// For Swift, through ios/TempleReader.h: 0 on success, otherwise the error's kind, its message
// in `message` – at most `capacity` bytes with the closing NUL – if that isn't null.

/// An object's size in bytes, into `*length`.
///
/// # Safety
/// `handle` is null or a NUL-terminated string, `length` points to a `u64`, and `message` is
/// null or points to `capacity` bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn temple_reader_object_length(
    handle: *const c_char,
    id: i64,
    length: *mut u64,
    message: *mut c_char,
    capacity: usize,
) -> i32 {
    // SAFETY: as the caller promises.
    match object_len(&unsafe { handle_from(handle) }, id) {
        Ok(len) => {
            unsafe { *length = len };
            0
        }
        Err(e) => unsafe { failed(&e, message, capacity) },
    }
}

/// Bytes `start..end` of an object, up to its end: `*count` of them at `*bytes`, which
/// [`temple_reader_free`] wipes and frees.
///
/// # Safety
/// As for [`temple_reader_object_length`]; `bytes` and `count` point to a pointer and a `usize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn temple_reader_read_object(
    handle: *const c_char,
    id: i64,
    start: u64,
    end: u64,
    bytes: *mut *mut u8,
    count: *mut usize,
    message: *mut c_char,
    capacity: usize,
) -> i32 {
    // SAFETY: as the caller promises.
    match read_object(&unsafe { handle_from(handle) }, id, start, end) {
        Ok(read) => {
            // Exactly as long as it is, for `temple_reader_free`; `read` is wiped as it drops.
            let copy = Box::<[u8]>::from(read.as_slice());
            unsafe {
                *count = copy.len();
                *bytes = Box::into_raw(copy).cast::<u8>();
            }
            0
        }
        Err(e) => unsafe { failed(&e, message, capacity) },
    }
}

/// Wipes and frees what [`temple_reader_read_object`] returned.
///
/// # Safety
/// `bytes` and `count` are as that call returned them, freed only once – or `bytes` is null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn temple_reader_free(bytes: *mut u8, count: usize) {
    if !bytes.is_null() {
        // SAFETY: a boxed slice of `count` bytes, given out by `temple_reader_read_object`.
        unsafe { Box::from_raw(std::ptr::slice_from_raw_parts_mut(bytes, count)) }.zeroize();
    }
}

/// A handle from C: one that isn't a string is none.
unsafe fn handle_from(handle: *const c_char) -> String {
    if handle.is_null() {
        return String::new();
    }
    // SAFETY: a NUL-terminated string, as the caller promises.
    unsafe { CStr::from_ptr(handle) }.to_str().unwrap_or_default().to_owned()
}

/// Writes an error's message for C, cut at a character to fit; returns its kind.
unsafe fn failed(error: &TempleError, message: *mut c_char, capacity: usize) -> i32 {
    if !message.is_null() && capacity > 0 {
        let text = error.to_string();
        let mut n = text.len().min(capacity - 1);
        while !text.is_char_boundary(n) {
            n -= 1;
        }
        // SAFETY: `message` holds `capacity` bytes, and `n` is below that.
        unsafe {
            std::ptr::copy_nonoverlapping(text.as_ptr().cast::<c_char>(), message, n);
            *message.add(n) = 0;
        }
    }
    kind(error)
}

/// For Java – and Kotlin – through android/…/TempleReader.java, whose arguments it has checked:
/// its native methods. They fail with an IOException whose message starts with the error's
/// kind, which TempleReader turns into a TempleReaderException: no class of Temple's is looked
/// up by name, which a shrinker could have renamed.
#[cfg(target_os = "android")]
mod java {
    use super::{kind, object_len, read_object};
    use crate::TempleError;
    use jni_sys::{JNIEnv, jbyte, jbyteArray, jclass, jint, jlong, jsize, jstring};
    use std::ffi::{CStr, CString};

    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn Java_de_schaefchens_temple_TempleReader_nativeObjectLength(
        env: *mut JNIEnv,
        _class: jclass,
        handle: jstring,
        id: jlong,
    ) -> jlong {
        // SAFETY: Java calls with its environment and a string.
        let len = unsafe { string(env, handle) }.and_then(|handle| object_len(&handle, id));
        match len {
            Ok(len) => jlong::try_from(len).unwrap_or(jlong::MAX),
            Err(e) => unsafe { throw(env, &e, 0) },
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn Java_de_schaefchens_temple_TempleReader_nativeReadObject(
        env: *mut JNIEnv,
        _class: jclass,
        handle: jstring,
        id: jlong,
        start: jlong,
        end: jlong,
    ) -> jbyteArray {
        // SAFETY: Java calls with its environment and a string; `start` and `end` aren't negative.
        let read = unsafe { string(env, handle) }.and_then(|handle| read_object(&handle, id, start as u64, end as u64));
        let read = match read {
            Ok(read) => read,
            Err(e) => return unsafe { throw(env, &e, std::ptr::null_mut()) },
        };
        let Ok(len) = jsize::try_from(read.len()) else {
            let e = TempleError::Io("the range is longer than a Java array holds".into());
            return unsafe { throw(env, &e, std::ptr::null_mut()) };
        };
        // SAFETY: an array of `len` bytes, filled from `read`; null if Java is out of memory, and throws.
        unsafe {
            let array = (**env).NewByteArray.unwrap()(env, len);
            if !array.is_null() {
                (**env).SetByteArrayRegion.unwrap()(env, array, 0, len, read.as_ptr().cast::<jbyte>());
            }
            array
        }
    }

    #[unsafe(no_mangle)]
    pub unsafe extern "system" fn Java_de_schaefchens_temple_TempleReader_nativeReadInto(
        env: *mut JNIEnv,
        _class: jclass,
        handle: jstring,
        id: jlong,
        position: jlong,
        buffer: jbyteArray,
        offset: jint,
        length: jint,
    ) -> jint {
        let (start, len) = (position as u64, length as u64);
        // SAFETY: Java calls with its environment and a string; `position` and `length` aren't negative.
        let read = unsafe { string(env, handle) }.and_then(|handle| read_object(&handle, id, start, start + len));
        match read {
            // SAFETY: Java checked that `offset` and `length` lie within `buffer`; `read` is no longer.
            Ok(read) => unsafe {
                (**env).SetByteArrayRegion.unwrap()(env, buffer, offset, read.len() as jsize, read.as_ptr().cast());
                read.len() as jint
            },
            Err(e) => unsafe { throw(env, &e, -1) },
        }
    }

    /// A Java string as Rust's: handles are ASCII.
    unsafe fn string(env: *mut JNIEnv, string: jstring) -> Result<String, TempleError> {
        // SAFETY: Java's environment, and a string it passed.
        unsafe {
            let chars = (**env).GetStringUTFChars.unwrap()(env, string, std::ptr::null_mut());
            if chars.is_null() {
                // Java is out of memory, and throws.
                return Err(TempleError::Io("out of memory".into()));
            }
            let text = CStr::from_ptr(chars).to_string_lossy().into_owned();
            (**env).ReleaseStringUTFChars.unwrap()(env, string, chars);
            Ok(text)
        }
    }

    /// Throws `error` as an IOException, "kind message" – unless Java throws already – and
    /// returns `value`, which Java ignores then.
    unsafe fn throw<T>(env: *mut JNIEnv, error: &TempleError, value: T) -> T {
        // SAFETY: Java's environment; a step that fails leaves an exception pending.
        unsafe {
            let jni = &**env;
            if jni.ExceptionCheck.unwrap()(env) != 0 {
                return value;
            }
            let class = jni.FindClass.unwrap()(env, c"java/io/IOException".as_ptr());
            if !class.is_null() {
                let message = CString::new(format!("{} {error}", kind(error)).replace('\0', "")).unwrap_or_default();
                jni.ThrowNew.unwrap()(env, class, message.as_ptr());
            }
            value
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::wait;
    use crate::{Vault, generate_words, open_vault};

    /// A vault of its own with one space, open, and in it an object of `len` bytes.
    fn space_with_object(name: &str, len: usize) -> (std::path::PathBuf, Arc<Vault>, Arc<Space>, i64, Vec<u8>) {
        // Any file of two zones takes a space – a small one, without a new vault's reserve.
        let path = std::env::temp_dir().join(format!("temple-ffi-{name}-{}.vault", std::process::id()));
        std::fs::write(&path, vec![0u8; 2 << 20]).unwrap();
        let vault = wait(open_vault(path.to_string_lossy().into_owned(), false)).unwrap();
        let space = wait(vault.clone().create_space(generate_words(), 0)).unwrap();
        let bytes: Vec<u8> = (0..len).map(|i| (i * 7 + i / 4093) as u8).collect();
        let id = wait(space.clone().put_object(bytes.clone())).unwrap();
        (path, vault, space, id, bytes)
    }

    fn is_locked<T: std::fmt::Debug>(result: Result<T, TempleError>) -> bool {
        matches!(result, Err(TempleError::Locked(_)))
    }

    #[test]
    fn native_code_reads_what_javascript_reads() {
        let (path, _vault, space, id, bytes) = space_with_object("reads", 300_000);
        let handle = wait(space.clone().native_reader()).unwrap();
        assert_eq!(object_len(&handle, id).unwrap(), bytes.len() as u64);
        let whole = read_object(&handle, id, 0, u64::MAX).unwrap();
        assert_eq!(*whole, bytes);
        assert_eq!(*whole, wait(space.clone().read_object(id, 0, u64::MAX)).unwrap());
        assert_eq!(*read_object(&handle, id, 5_000, 123_456).unwrap(), bytes[5_000..123_456]);
        assert_eq!(*read_object(&handle, id, 299_000, 400_000).unwrap(), bytes[299_000..]);
        assert!(read_object(&handle, id, 300_000, 400_000).unwrap().is_empty(), "from its end on: nothing");
        assert!(read_object(&handle, id, 500_000, 600_000).unwrap().is_empty(), "past its end: nothing");
        assert!(read_object(&handle, id, 10, 5).unwrap().is_empty(), "an empty range: nothing");
        assert!(matches!(read_object(&handle, id + 1, 0, 10), Err(TempleError::NoSuchObject(_))));
        wait(space.lock()).unwrap();
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn reads_are_refused_once_the_space_locks_or_the_handle_is_given_up() {
        let (path, vault, space, id, _) = space_with_object("refused", 10_000);
        let kept = wait(space.clone().native_reader()).unwrap();
        let released = wait(space.clone().native_reader()).unwrap();
        assert_ne!(kept, released, "every handle is its own");
        release_native_reader(released.clone());
        assert!(is_locked(read_object(&released, id, 0, 10)), "a handle given up");
        assert!(is_locked(object_len("not a handle", id)), "a handle never given out");
        assert_eq!(object_len(&kept, id).unwrap(), 10_000, "other handles still read");
        wait(space.clone().lock()).unwrap();
        assert!(is_locked(object_len(&kept, id)), "the space has locked");
        assert!(is_locked(read_object(&kept, id, 0, 10)));
        assert!(handles().iter().all(|(handle, _)| *handle != kept), "the lock took its handles");
        drop(vault);
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn a_handle_never_keeps_its_space_open() {
        let path = std::env::temp_dir().join(format!("temple-ffi-weak-{}.vault", std::process::id()));
        std::fs::write(&path, vec![0u8; 2 << 20]).unwrap();
        let vault = wait(open_vault(path.to_string_lossy().into_owned(), false)).unwrap();
        let words = generate_words();
        let space = wait(vault.clone().create_space(words.clone(), 0)).unwrap();
        let id = wait(space.clone().put_object(vec![1, 2, 3])).unwrap();
        let handle = wait(space.clone().native_reader()).unwrap();
        // Let go of without a lock, as when JavaScript's garbage collector gets to it: it locks.
        drop(space);
        assert!(is_locked(object_len(&handle, id)));
        // So its words open it again: no second opening was held.
        let space = wait(vault.clone().unlock(words.indices())).unwrap().expect("its words open it");
        assert!(is_locked(object_len(&handle, id)), "the old handle reads nothing of it");
        let handle = wait(space.clone().native_reader()).unwrap();
        assert_eq!(*read_object(&handle, id, 0, 10).unwrap(), [1, 2, 3]);
        wait(space.lock()).unwrap();
        std::fs::remove_file(&path).unwrap();
    }

    /// A media player's loader threads read while JavaScript queries, writes and reads too.
    #[test]
    fn reads_from_many_threads_while_javascript_uses_the_space() {
        let (path, _vault, space, id, bytes) = space_with_object("threads", 200_000);
        let handle = wait(space.clone().native_reader()).unwrap();
        let bytes = Arc::new(bytes);
        let readers: Vec<_> = (0..4u64)
            .map(|thread| {
                let (handle, bytes) = (handle.clone(), bytes.clone());
                std::thread::spawn(move || {
                    for round in 0..50u64 {
                        let start = (thread * 40_000 + round * 3_001) % 200_000;
                        let end = start + 16_384;
                        let read = read_object(&handle, id, start, end).unwrap();
                        assert_eq!(*read, bytes[start as usize..(end as usize).min(bytes.len())]);
                    }
                })
            })
            .collect();
        wait(space.clone().execute_batch("CREATE TABLE t (x)".into())).unwrap();
        for i in 0..50 {
            wait(space.clone().execute("INSERT INTO t VALUES (?)".into(), vec![crate::Value::Integer { value: i }]))
                .unwrap();
            let other = wait(space.clone().put_object(vec![i as u8; 5_000])).unwrap();
            assert_eq!(wait(space.clone().read_object(other, 0, 10)).unwrap(), [i as u8; 10]);
        }
        for reader in readers {
            reader.join().unwrap();
        }
        wait(space.lock()).unwrap();
        std::fs::remove_file(&path).unwrap();
    }

    /// A lock that comes while native code reads: each read either ends whole or fails as
    /// `Locked` – never anything else.
    #[test]
    fn a_lock_amid_reads_refuses_them() {
        let (path, _vault, space, id, bytes) = space_with_object("amid", 1 << 20);
        let handle = wait(space.clone().native_reader()).unwrap();
        let reader = std::thread::spawn(move || {
            let mut refused = false;
            for _ in 0..1_000 {
                match read_object(&handle, id, 0, u64::MAX) {
                    Ok(read) => assert!(!refused && *read == bytes, "a whole read, before the lock"),
                    Err(TempleError::Locked(_)) => refused = true,
                    Err(e) => panic!("{e}"),
                }
            }
            refused
        });
        std::thread::sleep(std::time::Duration::from_millis(20));
        wait(space.lock()).unwrap();
        assert!(reader.join().unwrap(), "the reads after the lock were refused");
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn swift_reads_through_c() {
        let (path, _vault, space, id, bytes) = space_with_object("c", 50_000);
        let handle = std::ffi::CString::new(wait(space.clone().native_reader()).unwrap()).unwrap();
        let mut message = [0 as c_char; 64];
        let (mut len, mut ptr, mut count) = (0u64, std::ptr::null_mut(), 0usize);
        unsafe {
            assert_eq!(temple_reader_object_length(handle.as_ptr(), id, &mut len, message.as_mut_ptr(), 64), 0);
            assert_eq!(len, 50_000);
            let read = temple_reader_read_object(
                handle.as_ptr(),
                id,
                1_000,
                9_000,
                &mut ptr,
                &mut count,
                message.as_mut_ptr(),
                64,
            );
            assert_eq!(read, 0);
            assert_eq!(std::slice::from_raw_parts(ptr, count), &bytes[1_000..9_000]);
            temple_reader_free(ptr, count);
            let read = temple_reader_read_object(
                handle.as_ptr(),
                id,
                60_000,
                70_000,
                &mut ptr,
                &mut count,
                message.as_mut_ptr(),
                64,
            );
            assert_eq!((read, count), (0, 0), "past its end: nothing");
            temple_reader_free(ptr, count);
            wait(space.lock()).unwrap();
            let read = temple_reader_object_length(handle.as_ptr(), id, &mut len, message.as_mut_ptr(), 64);
            assert_eq!(read, LOCKED);
            let text = CStr::from_ptr(message.as_ptr()).to_str().unwrap();
            assert!(text.len() == 63 && given_up().to_string().starts_with(text), "cut to fit: {text}");
            assert_eq!(temple_reader_object_length(std::ptr::null(), id, &mut len, std::ptr::null_mut(), 0), LOCKED);
        }
        std::fs::remove_file(&path).unwrap();
    }
}
