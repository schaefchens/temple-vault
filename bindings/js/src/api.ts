// Temple's TypeScript API, the same on React Native and the web.

/** A value SQLite stores. Integers come back as numbers while they're safe, beyond as bigints. */
export type SqlValue = null | number | bigint | string | Uint8Array;

/** A row of a query: its values by column name. */
export type Row = Record<string, SqlValue>;

export type ErrorKind =
  /** Not 12 indices below 2048, or the checksum fails – most likely a typo. */
  | 'InvalidWords'
  /** A phrase key isn't 32 bytes. */
  | 'InvalidKey'
  /** The file is too small to be a vault. */
  | 'NotAVault'
  /** The words open a space whose content fails its checks. */
  | 'Damaged'
  /** The space was written by a newer Temple. */
  | 'NewerFormat'
  /** Another device has opened the vault since this one did; Temple writes nothing more. */
  | 'ChangedElsewhere'
  /** A space of this vault is open; lock it first. */
  | 'SpaceOpen'
  /** The words already open a space in this vault. */
  | 'WordsInUse'
  | 'NoSuchObject'
  /** The vault was opened read-only, or the space salvaged: nothing is written. */
  | 'ReadOnly'
  /** Stopped by a lock that didn't wait for it. */
  | 'Interrupted'
  /** The space has locked; its words open it again. */
  | 'Locked'
  | 'NotFound'
  | 'AlreadyExists'
  | 'StorageFull'
  /** Another tab, worker or process has the vault open. */
  | 'InUse'
  /** A number that isn't a whole one, or not in range – caught before it reaches Temple. */
  | 'InvalidArgument'
  | 'Database'
  | 'Io';

export class TempleError extends Error {
  readonly kind: ErrorKind;

  constructor(kind: ErrorKind, message: string) {
    super(message);
    this.name = 'TempleError';
    this.kind = kind;
  }

  /** Whether `error` is Temple's – of `kind`, if given. */
  static is(error: unknown, kind?: ErrorKind): error is TempleError {
    return error instanceof TempleError && (kind === undefined || error.kind === kind);
  }
}

/**
 * Fresh words from `generateWords` – the only words a new vault, a new space or a word change takes: Argon2's salt is the
 * same in every vault, so words someone picked would be open to one precomputation against all of them.
 */
export interface NewWords {
  /** The 12 indices into a list of 2048, to show the user; the app brings the list. They open the space as typed words do. */
  readonly indices: number[];
}

/** Temple on one platform. A path is a file path on iOS and Android, a file name on the web. */
export interface Temple {
  /** 12 fresh words. */
  generateWords(): Promise<NewWords>;
  /** Whether 12 indices are words: each below 2048, the checksum right. */
  wordsAreValid(words: number[]): Promise<boolean>;
  /** A new vault with its first space, open; `room`: bytes of content to hold from the start. */
  createVault(path: string, words: NewWords, options?: { room?: number }): Promise<{ vault: Vault; space: Space }>;
  /** `readOnly`: nothing is written to the vault – its spaces open read-only and leave no trace; for read-only storage, or to look without one. */
  openVault(path: string, options?: { readOnly?: boolean }): Promise<Vault>;
  /**
   * Marks the file at `path` to be left out of backups – iCloud's and the computer's on iOS, where every backup is another
   * copy of the vault. Whether the system keeps such a mark: on Android the app's manifest leaves files out, and the web's
   * private files aren't backed up.
   */
  excludeFromBackup(path: string): Promise<boolean>;
}

/** One file holding any number of spaces, each opened by its own 12 words. */
export interface Vault {
  /** The space these words open, or null if they open none – the same work either way. */
  unlock(words: number[]): Promise<Space | null>;
  /** Quick unlock with a phrase key the app kept from `Space.phraseKey`. */
  unlockWithKey(key: Uint8Array): Promise<Space | null>;
  /** What can still be read of a damaged space, read-only – to copy into a new vault, or out. Writes nothing to the vault. */
  salvage(words: number[]): Promise<Space | null>;
  /** A new space under fresh words – for words that opened none, once the user confirmed: the words typed never become a space. */
  createSpace(words: NewWords, options?: { room?: number }): Promise<Space>;
  /** Lets go of the vault; its file closes once its spaces have locked too. */
  close(): void;
}

/** What one set of 12 words opens: a SQLite database with full-text search, and objects. */
export interface Space {
  /**
   * Runs a statement; returns how many rows it changed. Tables named `temple_…` are Temple's. A transaction is the whole
   * space's: until it ends, every call on the space – from anywhere in the app – is part of it.
   */
  execute(sql: string, params?: SqlValue[]): Promise<number>;
  /** Runs statements separated by semicolons, without parameters. */
  executeBatch(sql: string): Promise<void>;
  query<T extends Row = Row>(sql: string, params?: SqlValue[]): Promise<T[]>;
  /** Stores bytes as a new object – audio, a document – and returns its id; inside a transaction, it commits or rolls back with it. */
  putObject(content: Uint8Array): Promise<number>;
  /** A new object written piece by piece – content too large to hold at once, such as a download. */
  writeObject(): Promise<ObjectWriter>;
  /** An object's bytes from `start` to `end`, or to its end. */
  readObject(id: number, start?: number, end?: number): Promise<Uint8Array>;
  objectLength(id: number): Promise<number>;
  deleteObject(id: number): Promise<void>;
  /** 32 bytes for quick unlock, for the app to seal with the device's secure hardware. */
  phraseKey(): Promise<Uint8Array>;
  changedElsewhere(): Promise<boolean>;
  /** In a vault opened read-only, or salvaged. */
  isReadOnly(): Promise<boolean>;
  /** New words for this space; a kept phrase key stops working. Interrupted, both words open the space until the change is made again from the old ones. */
  changeWords(words: NewWords): Promise<void>;
  /** Words that open this space no more – the old ones after an interrupted change; whether they did. */
  forgetWords(words: number[]): Promise<boolean>;
  /** Removes every other space of the vault at once. */
  keepOnlyThis(): Promise<void>;
  /** A new vault holding a copy of this space: rebuilding a vault. Under the same words, or new ones after words leaked – the copy then opens only with those. */
  copyToNewVault(path: string, options?: { words?: NewWords }): Promise<Vault>;
  /** Copies this space into another vault, under the same words or new ones. */
  copyInto(vault: Vault, options?: { words?: NewWords }): Promise<void>;
  /** Locks the space: deleted values become unreadable, keys are wiped. It doesn't wait: on iOS and Android, work still running on the space fails as "Interrupted". */
  lock(): Promise<void>;
  isLocked(): Promise<boolean>;
}

/** An object written piece by piece. One never finished ends as its space locks, and leaves nothing behind once it opens again. */
export interface ObjectWriter {
  /** Adds the next piece. A piece that fails ends the object – it may be half written: begin it again. */
  write(bytes: Uint8Array): Promise<void>;
  /** Stores the object; returns its id. Inside a transaction, it commits or rolls back with it. */
  finish(): Promise<number>;
}
