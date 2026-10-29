//! Vaults on the web: files in the origin private file system, read and written through sync
//! access handles, which only a worker can open – so the module runs in one. A path is a
//! file name there. New vaults are written as `.<name>.temple-part` and renamed once complete, as
//! Temple does with files; a sync access handle keeps a file from being renamed, so the new
//! vault's first space locks for the rename and opens again with its phrase key.

use crate::{Space, TempleError, locked};
use js_sys::{Function, Promise, Reflect};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    FileSystemDirectoryHandle, FileSystemFileHandle, FileSystemGetFileOptions, FileSystemSyncAccessHandle,
    WorkerGlobalScope,
};

pub(crate) async fn create(
    name: &str,
    words: &temple_vault::NewWords,
    setup: temple_vault::Setup,
) -> Result<(temple_vault::Vault, temple_vault::Space), TempleError> {
    let dir = directory().await?;
    let (part, handle) = start(&dir, name).await?;
    let (vault, space) = match temple_vault::Vault::create_with_handle(handle, words, setup) {
        Ok(created) => created,
        Err(e) => {
            let _ = remove(&dir, &partial(name)).await;
            return Err(e.into());
        }
    };
    let key = space.phrase_key();
    space.lock()?;
    drop(vault);
    finish(&dir, &part, name).await?;
    let vault = open(name, false).await?;
    let space = vault.unlock_with_key(&key)?.ok_or_else(|| temple_vault::Error::Damaged)?;
    Ok((vault, space))
}

pub(crate) async fn open(name: &str, read_only: bool) -> Result<temple_vault::Vault, TempleError> {
    let file = file(&directory().await?, name, false).await?;
    Ok(temple_vault::Vault::open_with_handle(sync_handle(&file).await?, read_only)?)
}

pub(crate) async fn copy_to_new_vault(
    space: &Space,
    name: &str,
    new_words: Option<&temple_vault::NewWords>,
) -> Result<temple_vault::Vault, TempleError> {
    let dir = directory().await?;
    let (part, handle) = start(&dir, name).await?;
    // No await while the space is held: another call on it would find it taken. A space that
    // locked meanwhile leaves the handle to close – it would keep the file from being removed.
    let copied = match space.guard().as_ref() {
        Some(space) => space.copy_to_new_vault_with_handle(handle, new_words).map_err(TempleError::from),
        None => {
            handle.close();
            Err(locked())
        }
    };
    match copied {
        Ok(vault) => drop(vault),
        Err(e) => {
            let _ = remove(&dir, &partial(name)).await;
            return Err(e);
        }
    }
    finish(&dir, &part, name).await?;
    open(name, false).await
}

/// A fresh `.<name>.temple-part` and a handle to it, after removing what a closed tab left
/// there – unless another tab still writes it, whose handle keeps it from being removed.
/// Names ending in `.temple-part` are Temple's: another creation would remove them.
async fn start(
    dir: &FileSystemDirectoryHandle,
    name: &str,
) -> Result<(FileSystemFileHandle, FileSystemSyncAccessHandle), TempleError> {
    if name.to_ascii_lowercase().ends_with(".temple-part") {
        return Err(TempleError::Io("names ending in .temple-part are Temple's".into()));
    }
    match file(dir, name, false).await {
        Err(TempleError::NotFound(_)) => {}
        Ok(_) => return Err(TempleError::AlreadyExists("something exists at the vault's path".into())),
        Err(e) => return Err(e),
    }
    match remove(dir, &partial(name)).await {
        Ok(()) | Err(TempleError::NotFound(_)) => {}
        Err(e) => return Err(e),
    }
    let part = file(dir, &partial(name), true).await?;
    let handle = sync_handle(&part).await?;
    Ok((part, handle))
}

/// Gives the complete vault its name – `move()`, which not every browser has yet; WebKit
/// knows only the form with the directory.
async fn finish(dir: &FileSystemDirectoryHandle, part: &FileSystemFileHandle, name: &str) -> Result<(), TempleError> {
    let rename: Function = Reflect::get(part, &"move".into())
        .ok()
        .and_then(|f| f.dyn_into().ok())
        .ok_or_else(|| TempleError::Io("this browser can't rename files in its private file system".into()))?;
    let promise: Promise = rename.call2(part, dir, &name.into()).map_err(error)?.unchecked_into();
    JsFuture::from(promise).await.map_err(error)?;
    Ok(())
}

fn partial(name: &str) -> String {
    format!(".{name}.temple-part")
}

async fn directory() -> Result<FileSystemDirectoryHandle, TempleError> {
    let scope: WorkerGlobalScope =
        js_sys::global().dyn_into().map_err(|_| TempleError::Io("on the web, Temple runs in a worker".into()))?;
    Ok(JsFuture::from(scope.navigator().storage().get_directory()).await.map_err(error)?.unchecked_into())
}

async fn file(dir: &FileSystemDirectoryHandle, name: &str, create: bool) -> Result<FileSystemFileHandle, TempleError> {
    if name.is_empty() || name.contains('/') {
        return Err(TempleError::Io(format!("not a file name: {name:?}")));
    }
    let options = FileSystemGetFileOptions::new();
    options.set_create(create);
    let found = JsFuture::from(dir.get_file_handle_with_options(name, &options)).await.map_err(error)?;
    Ok(found.unchecked_into())
}

async fn sync_handle(file: &FileSystemFileHandle) -> Result<FileSystemSyncAccessHandle, TempleError> {
    Ok(JsFuture::from(file.create_sync_access_handle()).await.map_err(error)?.unchecked_into())
}

async fn remove(dir: &FileSystemDirectoryHandle, name: &str) -> Result<(), TempleError> {
    JsFuture::from(dir.remove_entry(name)).await.map_err(error)?;
    Ok(())
}

/// A DOMException as Temple's error: another handle's lock means another tab has the vault.
fn error(e: JsValue) -> TempleError {
    let name = Reflect::get(&e, &"name".into()).ok().and_then(|name| name.as_string()).unwrap_or_default();
    let message = Reflect::get(&e, &"message".into()).ok().and_then(|m| m.as_string()).unwrap_or(format!("{e:?}"));
    match name.as_str() {
        "NotFoundError" => TempleError::NotFound(message),
        "NoModificationAllowedError" | "InvalidStateError" => TempleError::InUse(message),
        "QuotaExceededError" => TempleError::StorageFull(message),
        _ => TempleError::Io(format!("{name}: {message}")),
    }
}
