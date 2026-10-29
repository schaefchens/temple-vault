//! Temple for TypeScript: the API uniffi-bindgen-react-native turns into bindings for React
//! Native on iOS and Android and for the web. Every call that touches a vault is async and
//! runs off the JS thread – natively on Temple's own thread, on the web in the worker that
//! hosts the module, where vaults live in the origin private file system.

use std::fmt;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use temple_vault::rusqlite::types::{ToSqlOutput, ValueRef};
use temple_vault::rusqlite::{self, Connection, ToSql, params_from_iter};
use zeroize::Zeroizing;

mod background;
#[cfg(target_arch = "wasm32")]
mod web;

uniffi::setup_scaffolding!("temple");

#[cfg(target_arch = "wasm32")]
extern crate uniffi_runtime_wasm as _;

/// What can go wrong, each with its message. Words that open no space aren't an error:
/// unlocking then gives nothing.
#[derive(Debug, uniffi::Error)]
#[uniffi(flat_error)]
pub enum TempleError {
    /// Not 12 indices below 2048, or the checksum fails – most likely a typo.
    InvalidWords(String),
    /// A phrase key isn't 32 bytes.
    InvalidKey(String),
    NotAVault(String),
    Damaged(String),
    NewerFormat(String),
    ChangedElsewhere(String),
    SpaceOpen(String),
    WordsInUse(String),
    NoSuchObject(String),
    /// The vault was opened read-only, or the space salvaged.
    ReadOnly(String),
    /// Stopped by a lock that didn't wait for it.
    Interrupted(String),
    /// The space has locked; its words open it again.
    Locked(String),
    NotFound(String),
    AlreadyExists(String),
    StorageFull(String),
    /// On the web: another tab or worker has the vault open.
    InUse(String),
    Database(String),
    Io(String),
}

impl fmt::Display for TempleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use TempleError::*;
        let (InvalidWords(message)
        | InvalidKey(message)
        | NotAVault(message)
        | Damaged(message)
        | NewerFormat(message)
        | ChangedElsewhere(message)
        | SpaceOpen(message)
        | WordsInUse(message)
        | NoSuchObject(message)
        | ReadOnly(message)
        | Interrupted(message)
        | Locked(message)
        | NotFound(message)
        | AlreadyExists(message)
        | StorageFull(message)
        | InUse(message)
        | Database(message)
        | Io(message)) = self;
        f.write_str(message)
    }
}

impl std::error::Error for TempleError {}

impl From<temple_vault::Error> for TempleError {
    fn from(e: temple_vault::Error) -> TempleError {
        let message = e.to_string();
        match e {
            temple_vault::Error::InvalidWords => TempleError::InvalidWords(message),
            temple_vault::Error::NotAVault => TempleError::NotAVault(message),
            temple_vault::Error::Damaged => TempleError::Damaged(message),
            temple_vault::Error::NewerFormat => TempleError::NewerFormat(message),
            temple_vault::Error::ChangedElsewhere => TempleError::ChangedElsewhere(message),
            temple_vault::Error::SpaceOpen => TempleError::SpaceOpen(message),
            temple_vault::Error::WordsInUse => TempleError::WordsInUse(message),
            temple_vault::Error::NoSuchObject => TempleError::NoSuchObject(message),
            temple_vault::Error::ReadOnly => TempleError::ReadOnly(message),
            temple_vault::Error::Interrupted => TempleError::Interrupted(message),
            temple_vault::Error::Database(e) => match e.sqlite_error_code() {
                Some(rusqlite::ErrorCode::OperationInterrupted) => TempleError::Interrupted(message),
                Some(rusqlite::ErrorCode::ReadOnly) => TempleError::ReadOnly(message),
                _ => TempleError::Database(message),
            },
            temple_vault::Error::Io(e) => match e.kind() {
                std::io::ErrorKind::NotFound => TempleError::NotFound(message),
                std::io::ErrorKind::AlreadyExists => TempleError::AlreadyExists(message),
                std::io::ErrorKind::StorageFull => TempleError::StorageFull(message),
                // Another process has the vault open.
                std::io::ErrorKind::ResourceBusy => TempleError::InUse(message),
                _ => TempleError::Io(message),
            },
        }
    }
}

/// A value SQLite stores.
#[derive(Clone, Debug, PartialEq, uniffi::Enum)]
pub enum Value {
    Null,
    Integer { value: i64 },
    Real { value: f64 },
    Text { value: String },
    Blob { value: Vec<u8> },
}

impl ToSql for Value {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self {
            Value::Null => ToSqlOutput::from(rusqlite::types::Null),
            Value::Integer { value } => ToSqlOutput::from(*value),
            Value::Real { value } => ToSqlOutput::from(*value),
            Value::Text { value } => ToSqlOutput::from(value.as_str()),
            Value::Blob { value } => ToSqlOutput::from(value.as_slice()),
        })
    }
}

impl From<ValueRef<'_>> for Value {
    fn from(value: ValueRef<'_>) -> Value {
        match value {
            ValueRef::Null => Value::Null,
            ValueRef::Integer(value) => Value::Integer { value },
            ValueRef::Real(value) => Value::Real { value },
            ValueRef::Text(text) => Value::Text { value: String::from_utf8_lossy(text).into_owned() },
            ValueRef::Blob(blob) => Value::Blob { value: blob.to_vec() },
        }
    }
}

/// What a query returned: its column names, and each row's values in their order.
#[derive(uniffi::Record)]
pub struct Rows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Value>>,
}

/// A new vault and its first space, open.
#[derive(uniffi::Record)]
pub struct Created {
    pub vault: Arc<Vault>,
    pub space: Arc<Space>,
}

/// 12 fresh words – the only words a new space or a word change takes: the same salt in every
/// vault leaves words someone picked open to one precomputation against all of them.
#[uniffi::export]
pub fn generate_words() -> Arc<NewWords> {
    Arc::new(NewWords(temple_vault::NewWords::generate()))
}

/// Fresh words from [`generate_words`]. They stay in Rust; the app shows their indices.
#[derive(uniffi::Object)]
pub struct NewWords(temple_vault::NewWords);

#[uniffi::export]
impl NewWords {
    /// The 12 indices into a list of 2048. Only the indices count: every phrase works with
    /// every wordlist, and the app brings the list.
    pub fn indices(&self) -> Vec<u16> {
        self.0.indices().to_vec()
    }
}

/// Whether 12 indices are words: each below 2048, and the checksum right.
#[uniffi::export]
pub fn words_are_valid(words: Vec<u16>) -> bool {
    to_words(words).is_ok()
}

/// A new vault at `path` with its first space, which comes back open; `room` is how many
/// bytes of content the space should hold from the start. On iOS and Android `path` is a
/// file path, on the web a file name in the origin private file system. Fails if something
/// is there.
#[uniffi::export]
pub async fn create_vault(path: String, words: Arc<NewWords>, room: u64) -> Result<Created, TempleError> {
    let setup = temple_vault::Setup { room };
    #[cfg(not(target_arch = "wasm32"))]
    let (vault, space) = background::run(move || Ok(temple_vault::Vault::create(&path, &words.0, setup)?)).await?;
    #[cfg(target_arch = "wasm32")]
    let (vault, space) = web::create(&path, &words.0, setup).await?;
    Ok(Created { vault: Arc::new(Vault(vault)), space: Space::new(space) })
}

/// Opens the vault at `path`, named as for [`create_vault`]. Read-only, nothing is written to
/// it: its spaces open read-only, and leave no trace – for a vault on read-only storage, or to
/// look without one.
#[uniffi::export]
pub async fn open_vault(path: String, read_only: bool) -> Result<Arc<Vault>, TempleError> {
    #[cfg(not(target_arch = "wasm32"))]
    let vault = background::run(move || match read_only {
        true => Ok(temple_vault::Vault::open_read_only(&path)?),
        false => Ok(temple_vault::Vault::open(&path)?),
    })
    .await?;
    #[cfg(target_arch = "wasm32")]
    let vault = web::open(&path, read_only).await?;
    Ok(Arc::new(Vault(vault)))
}

/// Marks the file at `path` to be left out of backups – on iOS, iCloud's and the computer's;
/// every backup would be another copy of the vault. Whether the system keeps such a mark: on
/// Android, the app's manifest leaves files out, and the web's private files aren't backed up.
#[uniffi::export]
pub async fn exclude_from_backup(path: String) -> Result<bool, TempleError> {
    #[cfg(not(target_arch = "wasm32"))]
    return background::run(move || Ok(temple_vault::Vault::exclude_from_backup(&path)?)).await;
    #[cfg(target_arch = "wasm32")]
    return {
        let _ = path;
        Ok(false)
    };
}

/// A vault: one file holding any number of spaces, each opened by its own 12 words. Its file
/// stays open while this object or one of its spaces lives.
#[derive(uniffi::Object)]
pub struct Vault(temple_vault::Vault);

#[uniffi::export]
impl Vault {
    /// The space these words open, or nothing if they open none – the same work either way.
    pub async fn unlock(self: Arc<Self>, words: Vec<u16>) -> Result<Option<Arc<Space>>, TempleError> {
        let words = to_words(words)?;
        let space = background::run(move || Ok(self.0.unlock(&words)?)).await?;
        Ok(space.map(Space::new))
    }

    /// Quick unlock: like `unlock`, with a phrase key the app kept.
    pub async fn unlock_with_key(self: Arc<Self>, key: Vec<u8>) -> Result<Option<Arc<Space>>, TempleError> {
        let key = to_phrase_key(key)?;
        let space = background::run(move || Ok(self.0.unlock_with_key(&key)?)).await?;
        Ok(space.map(Space::new))
    }

    /// What can still be read of a damaged space, read-only – its blocks checked, its tag table
    /// not: to copy into a new vault, or out. Writes nothing to the vault.
    pub async fn salvage(self: Arc<Self>, words: Vec<u16>) -> Result<Option<Arc<Space>>, TempleError> {
        let words = to_words(words)?;
        let space = background::run(move || Ok(self.0.salvage(&words)?)).await?;
        Ok(space.map(Space::new))
    }

    /// A new space under fresh words, which comes back open – for words that opened none, once
    /// the app confirmed with the user, so that a typo leaves no trace.
    pub async fn create_space(self: Arc<Self>, words: Arc<NewWords>, room: u64) -> Result<Arc<Space>, TempleError> {
        let space = background::run(move || Ok(self.0.create_space(&words.0, temple_vault::Setup { room })?)).await?;
        Ok(Space::new(space))
    }
}

/// An open space: what one set of 12 words opens. Lock it when done – one the app merely
/// lets go of locks when the garbage collector gets to it, and keeps its vault busy until then.
#[derive(uniffi::Object)]
pub struct Space {
    space: Mutex<Option<temple_vault::Space>>,
    /// Stops the work running, for a lock.
    interrupt: temple_vault::Interrupt,
}

impl Space {
    fn new(space: temple_vault::Space) -> Arc<Space> {
        let interrupt = space.interrupt_handle();
        Arc::new(Space { space: Mutex::new(Some(space)), interrupt })
    }

    /// The space, or nothing once it has locked. A panic while it was held changes nothing
    /// Temple relies on: it checks what it reads.
    pub(crate) fn guard(&self) -> MutexGuard<'_, Option<temple_vault::Space>> {
        self.space.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Runs `work` on the open space, off the JS thread.
    async fn with<T: Send + 'static>(
        self: Arc<Self>,
        work: impl FnOnce(&mut temple_vault::Space) -> Result<T, TempleError> + Send + 'static,
    ) -> Result<T, TempleError> {
        background::run(move || work(self.guard().as_mut().ok_or_else(locked)?)).await
    }
}

#[uniffi::export]
impl Space {
    /// Runs one statement with its parameters; returns how many rows it changed. Tables named
    /// `temple_…` are Temple's; pragmas that would break the space's guarantees are refused.
    pub async fn execute(self: Arc<Self>, sql: String, params: Vec<Value>) -> Result<u64, TempleError> {
        self.with(move |space| Ok(checked(space, space.db().execute(&sql, params_from_iter(&params)))? as u64)).await
    }

    /// Runs statements separated by semicolons, without parameters.
    pub async fn execute_batch(self: Arc<Self>, sql: String) -> Result<(), TempleError> {
        self.with(move |space| checked(space, space.db().execute_batch(&sql))).await
    }

    /// Runs a query with its parameters; returns every row.
    pub async fn query(self: Arc<Self>, sql: String, params: Vec<Value>) -> Result<Rows, TempleError> {
        self.with(move |space| checked(space, query(space.db(), &sql, &params))).await
    }

    /// Stores `content` as a new object – audio, a document – and returns its id; inside a
    /// transaction, it commits or rolls back with it.
    pub async fn put_object(self: Arc<Self>, content: Vec<u8>) -> Result<i64, TempleError> {
        let content = Zeroizing::new(content);
        self.with(move |space| Ok(space.put_object(content.as_slice())?)).await
    }

    /// A new object written piece by piece – content too large for one buffer, such as a
    /// download – stored once finished.
    pub async fn begin_object(self: Arc<Self>) -> Result<Arc<ObjectWriter>, TempleError> {
        let writer = self.clone().with(|space| Ok(space.begin_object()?)).await?;
        Ok(Arc::new(ObjectWriter { space: self, writer: Mutex::new(Some(writer)) }))
    }

    /// The bytes of an object from `start` up to `end`, or up to its end if that's further.
    pub async fn read_object(self: Arc<Self>, id: i64, start: u64, end: u64) -> Result<Vec<u8>, TempleError> {
        self.with(move |space| Ok(space.read_object(id, start..end)?)).await
    }

    /// An object's size in bytes.
    pub async fn object_len(self: Arc<Self>, id: i64) -> Result<u64, TempleError> {
        self.with(move |space| Ok(space.object_len(id)?)).await
    }

    /// Deletes an object: its key goes with its row.
    pub async fn delete_object(self: Arc<Self>, id: i64) -> Result<(), TempleError> {
        self.with(move |space| Ok(space.delete_object(id)?)).await
    }

    /// The phrase key, 32 bytes, for the app to seal with the device's secure hardware.
    pub async fn phrase_key(self: Arc<Self>) -> Result<Vec<u8>, TempleError> {
        self.with(|space| Ok(space.phrase_key().as_bytes().to_vec())).await
    }

    /// Whether another device has opened the vault since this one did: Temple then writes
    /// nothing more.
    pub async fn changed_elsewhere(self: Arc<Self>) -> Result<bool, TempleError> {
        self.with(|space| Ok(space.changed_elsewhere())).await
    }

    /// Whether the space is read-only: in a vault opened read-only, or salvaged.
    pub async fn is_read_only(self: Arc<Self>) -> Result<bool, TempleError> {
        self.with(|space| Ok(space.is_read_only())).await
    }

    /// New words for this space; a phrase key kept for quick unlock stops working. Interrupted,
    /// both words open the space until the change is made again from the old ones, or the
    /// old ones are forgotten from the new.
    pub async fn change_words(self: Arc<Self>, words: Arc<NewWords>) -> Result<(), TempleError> {
        self.with(move |space| Ok(space.change_words(&words.0)?)).await
    }

    /// Words that open this space no more; whether they did. Not the words it was opened with.
    pub async fn forget_words(self: Arc<Self>, words: Vec<u16>) -> Result<bool, TempleError> {
        let words = to_words(words)?;
        self.with(move |space| Ok(space.forget_words(&words)?)).await
    }

    /// Keeps only this space: every other space of the vault is gone at once.
    pub async fn keep_only_this(self: Arc<Self>) -> Result<(), TempleError> {
        self.with(|space| Ok(space.keep_only_this()?)).await
    }

    /// A new vault at `path`, named as for [`create_vault`], holding a copy of this space: the
    /// first step of rebuilding a vault. Under the same words – or under `new_words` after
    /// words leaked: the copy opens only with those, under a fresh master key.
    pub async fn copy_to_new_vault(
        self: Arc<Self>,
        path: String,
        new_words: Option<Arc<NewWords>>,
    ) -> Result<Arc<Vault>, TempleError> {
        #[cfg(not(target_arch = "wasm32"))]
        let vault = self
            .with(move |space| Ok(space.copy_to_new_vault(&path, new_words.as_ref().map(|words| &words.0))?))
            .await?;
        #[cfg(target_arch = "wasm32")]
        let vault = web::copy_to_new_vault(&self, &path, new_words.as_ref().map(|words| &words.0)).await?;
        Ok(Arc::new(Vault(vault)))
    }

    /// Copies this space into another vault, under the same words or `new_words`: the next
    /// steps of rebuilding a vault, with hidden spaces.
    pub async fn copy_into(
        self: Arc<Self>,
        vault: Arc<Vault>,
        new_words: Option<Arc<NewWords>>,
    ) -> Result<(), TempleError> {
        self.with(move |space| Ok(space.copy_into(&vault.0, new_words.as_ref().map(|words| &words.0))?)).await
    }

    /// Locks the space: deleted values become unreadable, keys and everything decrypted are
    /// wiped. Without waiting: work running on the space stops, failing as `Interrupted`, and
    /// the lock goes before every call still waiting – on iOS with time to finish once the app
    /// moves to the background. Nothing happens if it has locked already.
    pub async fn lock(self: Arc<Self>) -> Result<(), TempleError> {
        self.interrupt.interrupt();
        background::run_first(move || match self.guard().take() {
            Some(space) => Ok(space.lock()?),
            None => Ok(()),
        })
        .await
    }

    /// Whether the space has locked.
    pub async fn is_locked(self: Arc<Self>) -> Result<bool, TempleError> {
        background::run(move || Ok(self.guard().is_none())).await
    }
}

/// An object written piece by piece, from [`Space::begin_object`]. One never finished leaves
/// nothing behind once its space opens again.
#[derive(uniffi::Object)]
pub struct ObjectWriter {
    space: Arc<Space>,
    writer: Mutex<Option<temple_vault::ObjectWriter>>,
}

#[uniffi::export]
impl ObjectWriter {
    /// Adds the next piece. A piece that fails ends the object: begin it again.
    pub async fn write(self: Arc<Self>, bytes: Vec<u8>) -> Result<(), TempleError> {
        let bytes = Zeroizing::new(bytes);
        self.space
            .clone()
            .with(move |space| {
                let mut writer = self.writer();
                Ok(space.write_object(writer.as_mut().ok_or_else(finished)?, &bytes)?)
            })
            .await
    }

    /// Stores the object; returns its id. Inside a transaction, it commits or rolls back with it.
    pub async fn finish(self: Arc<Self>) -> Result<i64, TempleError> {
        self.space
            .clone()
            .with(move |space| {
                let writer = self.writer().take().ok_or_else(finished)?;
                Ok(space.finish_object(writer)?)
            })
            .await
    }
}

impl ObjectWriter {
    fn writer(&self) -> MutexGuard<'_, Option<temple_vault::ObjectWriter>> {
        self.writer.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

fn finished() -> TempleError {
    TempleError::NoSuchObject("the object is finished".into())
}

/// Words from the app's indices, which are wiped once read.
fn to_words(indices: Vec<u16>) -> Result<temple_vault::Words, TempleError> {
    let indices = Zeroizing::new(indices);
    let indices: [u16; 12] = indices.as_slice().try_into().map_err(|_| temple_vault::Error::InvalidWords)?;
    Ok(temple_vault::Words::from_indices(indices)?)
}

/// A phrase key from the app's bytes, which are wiped once read.
fn to_phrase_key(bytes: Vec<u8>) -> Result<temple_vault::PhraseKey, TempleError> {
    let bytes = Zeroizing::new(bytes);
    let bytes: [u8; 32] =
        bytes.as_slice().try_into().map_err(|_| TempleError::InvalidKey("a phrase key is 32 bytes".into()))?;
    Ok(temple_vault::PhraseKey::from_bytes(bytes))
}

pub(crate) fn locked() -> TempleError {
    TempleError::Locked("the space has locked".into())
}

/// SQLite's result, its I/O errors told apart: another device opened the vault, a page failed
/// its check, the disk is full.
fn checked<T>(space: &temple_vault::Space, result: rusqlite::Result<T>) -> Result<T, TempleError> {
    result.map_err(|e| space.explain(e).into())
}

fn query(db: &Connection, sql: &str, params: &[Value]) -> rusqlite::Result<Rows> {
    let mut statement = db.prepare(sql)?;
    let columns: Vec<String> = statement.column_names().map(str::to_owned).collect();
    let mut cursor = statement.query(params_from_iter(params))?;
    let mut rows = Vec::new();
    while let Some(row) = cursor.next()? {
        rows.push((0..columns.len()).map(|i| row.get_ref(i).map(Value::from)).collect::<rusqlite::Result<_>>()?);
    }
    Ok(Rows { columns, rows })
}
