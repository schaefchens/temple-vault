//! Temple: vaults with any number of spaces, each opened by its own 12 words.
//!
//! A vault is one file. Without words it is random bytes; with one space's words, nothing
//! shows whether other spaces exist. How a vault behaves and why: `docs/design.md`; its
//! byte layout: `docs/format.md`.
//!
//! ```no_run
//! use temple_vault::{Setup, Vault, Words};
//!
//! let words = Words::generate();
//! let (vault, space) = Vault::create("reading.vault", &words, Setup::default())?;
//! space.db().execute("CREATE TABLE note (body TEXT)", [])?;
//! space.lock()?;
//!
//! let space = vault.unlock(&words)?.expect("these words open a space");
//! # Ok::<(), temple_vault::Error>(())
//! ```
//!
//! Temple brings its own SQLite – re-exported as [`rusqlite`] – and must be the first to use
//! it in a process: it installs a heap that wipes whatever SQLite frees, which SQLite only
//! takes before it starts, and turns URI filenames off. A space's connection carries Temple's
//! authorizer and log hook; setting others takes Temple's guards away.

mod crypto;
mod device;
mod error;
#[cfg(any(test, feature = "fuzzing"))]
#[doc(hidden)]
pub mod fuzzing;
mod keys;
mod noise;
mod slots;
mod space;
mod vault;
mod words;

#[cfg(test)]
mod tests;

pub use error::Error;
pub use keys::PhraseKey;
pub use rusqlite;
pub use space::{ObjectId, Space};
pub use vault::{Setup, Vault};
pub use words::Words;

/// 1 MiB: vaults grow by whole zones.
const ZONE: u64 = 1 << 20;
/// 4 KiB: spaces encrypt in blocks.
const BLOCK: usize = 4096;
