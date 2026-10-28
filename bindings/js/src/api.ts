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
  /** Objects are written outside transactions. */
  | 'InTransaction'
  /** The space has locked; its words open it again. */
  | 'Locked'
  | 'NotFound'
  | 'AlreadyExists'
  | 'StorageFull'
  /** On the web: another tab or worker has the vault open. */
  | 'InUse'
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

/** Temple on one platform. A path is a file path on iOS and Android, a file name on the web. */
export interface Temple {
  /** 12 fresh words, as indices into a list of 2048; the app brings the list. */
  generateWords(): Promise<number[]>;
  /** Whether 12 indices are words: each below 2048, the checksum right. */
  wordsAreValid(words: number[]): Promise<boolean>;
  /** A new vault with its first space, open; `room`: bytes of content to hold from the start. */
  createVault(path: string, words: number[], options?: { room?: number }): Promise<{ vault: Vault; space: Space }>;
  openVault(path: string): Promise<Vault>;
}

/** One file holding any number of spaces, each opened by its own 12 words. */
export interface Vault {
  /** The space these words open, or null if they open none – the same work either way. */
  unlock(words: number[]): Promise<Space | null>;
  /** Quick unlock with a phrase key the app kept from `Space.phraseKey`. */
  unlockWithKey(key: Uint8Array): Promise<Space | null>;
  /** A new space for words that open none – once the user confirmed them, so a typo leaves no trace. */
  createSpace(words: number[], options?: { room?: number }): Promise<Space>;
  /** Lets go of the vault; its file closes once its spaces have locked too. */
  close(): void;
}

/** What one set of 12 words opens: a SQLite database with full-text search, and objects. */
export interface Space {
  /** Runs a statement; returns how many rows it changed. Tables named `temple_…` are Temple's. */
  execute(sql: string, params?: SqlValue[]): Promise<number>;
  /** Runs statements separated by semicolons, without parameters. */
  executeBatch(sql: string): Promise<void>;
  query<T extends Row = Row>(sql: string, params?: SqlValue[]): Promise<T[]>;
  /** Stores bytes as a new object – audio, a document – outside any transaction; returns its id. */
  putObject(content: Uint8Array): Promise<number>;
  /** An object's bytes from `start` to `end`, or to its end. */
  readObject(id: number, start?: number, end?: number): Promise<Uint8Array>;
  objectLength(id: number): Promise<number>;
  deleteObject(id: number): Promise<void>;
  /** 32 bytes for quick unlock, for the app to seal with the device's secure hardware. */
  phraseKey(): Promise<Uint8Array>;
  changedElsewhere(): Promise<boolean>;
  /** New words for this space; a kept phrase key stops working. */
  changeWords(words: number[]): Promise<void>;
  /** Removes every other space of the vault at once. */
  keepOnlyThis(): Promise<void>;
  /** A new vault holding a copy of this space under the same words: rebuilding a vault. */
  copyToNewVault(path: string): Promise<Vault>;
  /** Copies this space into another vault under the same words. */
  copyInto(vault: Vault): Promise<void>;
  /** Locks the space: deleted values become unreadable, keys are wiped. */
  lock(): Promise<void>;
  isLocked(): Promise<boolean>;
}
