use std::{fmt, io};

/// What can go wrong. Words that open no space aren't an error: unlocking then gives `None`.
#[derive(Debug)]
pub enum Error {
    /// An index isn't below 2048 or the checksum fails – most likely a typo.
    InvalidWords,
    /// The file can't be a vault: it's smaller than two zones (2 MiB).
    NotAVault,
    /// The words open a space whose content fails its checks. Temple never repairs a vault
    /// by writing to it: a repair could overwrite spaces it can't see.
    Damaged,
    /// The space was written by a newer version of Temple.
    NewerFormat,
    /// Another device has opened the vault since this one did; Temple has stopped writing
    /// to it. Writes through the database then fail with SQLite's I/O error.
    ChangedElsewhere,
    /// A space of this vault is open; lock it first.
    SpaceOpen,
    /// The words already open a space in this vault.
    WordsInUse,
    /// The space has no object with this id.
    NoSuchObject,
    Database(rusqlite::Error),
    Io(io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::InvalidWords => f.write_str("these aren't valid words"),
            Error::NotAVault => f.write_str("not a vault"),
            Error::Damaged => f.write_str("the space is damaged"),
            Error::NewerFormat => f.write_str("the space was written by a newer version of Temple"),
            Error::ChangedElsewhere => f.write_str("another device has opened the vault"),
            Error::SpaceOpen => f.write_str("a space of this vault is open"),
            Error::WordsInUse => f.write_str("these words already open a space"),
            Error::NoSuchObject => f.write_str("no such object"),
            Error::Database(e) => write!(f, "database: {e}"),
            Error::Io(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for Error {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Error::Database(e) => Some(e),
            Error::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Error {
        Error::Io(e)
    }
}

impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Error {
        Error::Database(e)
    }
}
