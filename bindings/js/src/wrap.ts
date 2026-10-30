// The API of api.ts made from the bindings uniffi-bindgen-react-native generates – the same
// module on React Native and in the web's worker. Plain JS values in, plain JS values out:
// numbers for ids and sizes, Uint8Array for bytes, null for nothing, TempleError for errors.

import {
  TempleError,
  type ErrorKind,
  type NewWords,
  type ObjectWriter,
  type Row,
  type Space,
  type SqlValue,
  type Temple,
  type Vault,
} from './api';

/** What `wrap` needs of the generated module; `import * as binding from './generated/temple'`. */
export interface Binding {
  generateWords(): BindingNewWords;
  wordsAreValid(words: number[]): boolean;
  createVault(path: string, words: BindingNewWords, room: bigint): Promise<{ vault: BindingVault; space: BindingSpace }>;
  openVault(path: string, readOnly: boolean): Promise<BindingVault>;
  excludeFromBackup(path: string): Promise<boolean>;
  Value: {
    Null: new () => unknown;
    Integer: new (inner: { value: bigint }) => unknown;
    Real: new (inner: { value: number }) => unknown;
    Text: new (inner: { value: string }) => unknown;
    Blob: new (inner: { value: ArrayBuffer }) => unknown;
  };
  TempleError: { instanceOf(error: unknown): boolean };
}

interface Destroyable {
  uniffiDestroy?(): void;
}

interface BindingNewWords extends Destroyable {
  indices(): number[];
}

interface BindingVault extends Destroyable {
  unlock(words: number[]): Promise<BindingSpace | undefined>;
  unlockWithKey(key: ArrayBuffer): Promise<BindingSpace | undefined>;
  salvage(words: number[]): Promise<BindingSpace | undefined>;
  createSpace(words: BindingNewWords, room: bigint): Promise<BindingSpace>;
}

interface BindingSpace extends Destroyable {
  execute(sql: string, params: unknown[]): Promise<bigint>;
  executeBatch(sql: string): Promise<void>;
  query(sql: string, params: unknown[]): Promise<{ columns: string[]; rows: BindingValue[][] }>;
  putObject(content: ArrayBuffer): Promise<bigint>;
  beginObject(): Promise<BindingObjectWriter>;
  readObject(id: bigint, start: bigint, end: bigint): Promise<ArrayBuffer>;
  objectLen(id: bigint): Promise<bigint>;
  deleteObject(id: bigint): Promise<void>;
  phraseKey(): Promise<ArrayBuffer>;
  changedElsewhere(): Promise<boolean>;
  isReadOnly(): Promise<boolean>;
  changeWords(words: BindingNewWords): Promise<void>;
  forgetWords(words: number[]): Promise<boolean>;
  keepOnlyThis(): Promise<void>;
  copyToNewVault(path: string, words: BindingNewWords | undefined): Promise<BindingVault>;
  copyInto(vault: BindingVault, words: BindingNewWords | undefined): Promise<void>;
  lock(): Promise<void>;
}

interface BindingObjectWriter extends Destroyable {
  write(bytes: ArrayBuffer): Promise<void>;
  finish(): Promise<bigint>;
}

type BindingValue = { tag: string; inner?: { value: unknown } };

const END = 2n ** 64n - 1n;

export function wrap(binding: Binding): Temple {
  /** Runs a call, its errors as TempleError. */
  const run = async <T>(call: () => Promise<T>): Promise<T> => {
    try {
      return await call();
    } catch (error) {
      if (!binding.TempleError.instanceOf(error)) throw error;
      const { tag, message } = error as { tag: ErrorKind; message: string };
      throw new TempleError(tag, message.replace(/^TempleError\.\w+: /, ''));
    }
  };

  const lower = (value: SqlValue): unknown => {
    const { Value } = binding;
    if (value === null || value === undefined) return new Value.Null();
    if (typeof value === 'bigint') {
      if (value < -(2n ** 63n) || value >= 2n ** 63n) {
        throw new TempleError('InvalidArgument', `an integer parameter is outside SQLite's 64-bit range: ${value}`);
      }
      return new Value.Integer({ value });
    }
    if (typeof value === 'number') {
      return Number.isSafeInteger(value) ? new Value.Integer({ value: BigInt(value) }) : new Value.Real({ value });
    }
    if (typeof value === 'string') return new Value.Text({ value });
    if (value instanceof Uint8Array) return new Value.Blob({ value: buffer(value) });
    throw new TempleError('Database', `SQLite can't store a ${typeof value}`);
  };

  class WrappedVault implements Vault {
    #inner: BindingVault | null;

    constructor(inner: BindingVault) {
      this.#inner = inner;
    }

    /** The open vault; once closed, calls fail as a TempleError rather than a raw binding error. */
    get inner(): BindingVault {
      if (!this.#inner) throw new TempleError('NotFound', 'the vault is closed');
      return this.#inner;
    }

    async unlock(words: number[]) {
      const space = await run(() => this.inner.unlock(valid(words)));
      return space ? new WrappedSpace(space) : null;
    }

    async unlockWithKey(key: Uint8Array) {
      const space = await run(() => this.inner.unlockWithKey(buffer(key)));
      return space ? new WrappedSpace(space) : null;
    }

    async salvage(words: number[]) {
      const space = await run(() => this.inner.salvage(valid(words)));
      return space ? new WrappedSpace(space) : null;
    }

    async createSpace(words: NewWords, options?: { room?: number }) {
      return new WrappedSpace(await run(() => this.inner.createSpace(fresh(words), room(options))));
    }

    close() {
      this.#inner?.uniffiDestroy?.();
      this.#inner = null;
    }
  }

  class WrappedSpace implements Space {
    #inner: BindingSpace | null;
    #locking: Promise<void> | null = null;

    constructor(inner: BindingSpace) {
      this.#inner = inner;
    }

    /** The open space; once it has locked, its Rust object is gone too. */
    get #space(): BindingSpace {
      if (!this.#inner) throw new TempleError('Locked', 'the space has locked');
      return this.#inner;
    }

    async execute(sql: string, params: SqlValue[] = []) {
      return Number(await run(() => this.#space.execute(sql, Array.from(params, lower))));
    }

    executeBatch(sql: string) {
      return run(() => this.#space.executeBatch(sql));
    }

    async query<T extends Row = Row>(sql: string, params: SqlValue[] = []) {
      const { columns, rows } = await run(() => this.#space.query(sql, Array.from(params, lower)));
      return rows.map((row) => Object.fromEntries(columns.map((column, i) => [column, lift(row[i])])) as T);
    }

    async putObject(content: Uint8Array) {
      return Number(await run(() => this.#space.putObject(buffer(content))));
    }

    async writeObject() {
      return new WrappedObjectWriter(await run(() => this.#space.beginObject()));
    }

    async readObject(id: number, start = 0, end?: number) {
      const to = end === undefined ? END : whole(end, 'end');
      return new Uint8Array(await run(() => this.#space.readObject(whole(id, 'id', true), whole(start, 'start'), to)));
    }

    async objectLength(id: number) {
      return Number(await run(() => this.#space.objectLen(whole(id, 'id', true))));
    }

    async deleteObject(id: number) {
      return run(() => this.#space.deleteObject(whole(id, 'id', true)));
    }

    async phraseKey() {
      return new Uint8Array(await run(() => this.#space.phraseKey()));
    }

    changedElsewhere() {
      return run(() => this.#space.changedElsewhere());
    }

    isReadOnly() {
      return run(() => this.#space.isReadOnly());
    }

    async changeWords(words: NewWords) {
      return run(() => this.#space.changeWords(fresh(words)));
    }

    async forgetWords(words: number[]) {
      return run(() => this.#space.forgetWords(valid(words)));
    }

    keepOnlyThis() {
      return run(() => this.#space.keepOnlyThis());
    }

    async copyToNewVault(path: string, options?: { words?: NewWords }) {
      const words = options?.words && fresh(options.words);
      return new WrappedVault(await run(() => this.#space.copyToNewVault(path, words)));
    }

    async copyInto(vault: Vault, options?: { words?: NewWords }) {
      if (!(vault instanceof WrappedVault)) throw new TempleError('NotFound', 'not a vault of this Temple');
      const words = options?.words && fresh(options.words);
      return run(() => this.#space.copyInto(vault.inner, words));
    }

    /** Even when locking fails – another device opened the vault – the space is closed. Calls while it locks share that lock. */
    lock() {
      const space = this.#inner;
      if (!this.#locking && space) {
        this.#locking = run(() => space.lock()).finally(() => {
          this.#inner = null;
          this.#locking = null;
          space.uniffiDestroy?.();
        });
      }
      return this.#locking ?? Promise.resolve();
    }

    async isLocked() {
      return this.#inner === null;
    }
  }

  class WrappedObjectWriter implements ObjectWriter {
    #inner: BindingObjectWriter | null;

    constructor(inner: BindingObjectWriter) {
      this.#inner = inner;
    }

    /** The writer, until it finished – its Rust object is gone then. */
    get #writer(): BindingObjectWriter {
      if (!this.#inner) throw new TempleError('NoSuchObject', 'the object is finished');
      return this.#inner;
    }

    write(bytes: Uint8Array) {
      return run(() => this.#writer.write(buffer(bytes)));
    }

    async finish() {
      const writer = this.#writer;
      this.#inner = null;
      try {
        return Number(await run(() => writer.finish()));
      } finally {
        writer.uniffiDestroy?.();
      }
    }
  }

  return {
    async generateWords() {
      return new WrappedNewWords(binding.generateWords());
    },
    async wordsAreValid(words: number[]) {
      const checked = indices(words);
      return checked !== null && binding.wordsAreValid(checked);
    },
    async createVault(path: string, words: NewWords, options?: { room?: number }) {
      const { vault, space } = await run(() => binding.createVault(path, fresh(words), room(options)));
      return { vault: new WrappedVault(vault), space: new WrappedSpace(space) };
    },
    async openVault(path: string, options?: { readOnly?: boolean }) {
      return new WrappedVault(await run(() => binding.openVault(path, options?.readOnly ?? false)));
    },
    excludeFromBackup(path: string) {
      return run(() => binding.excludeFromBackup(path));
    },
  };
}

/** Words from `generateWords`, kept in Rust: their indices are only for showing. */
class WrappedNewWords implements NewWords {
  readonly indices: number[];

  constructor(readonly inner: BindingNewWords) {
    this.indices = inner.indices();
  }
}

/** New words as they cross into Rust – only those Temple made: typed or picked words never become a space. */
function fresh(words: NewWords): BindingNewWords {
  if (!(words instanceof WrappedNewWords)) {
    throw new TempleError('InvalidWords', 'new words come from generateWords, never from the user');
  }
  return words.inner;
}

function room(options?: { room?: number }): bigint {
  return whole(options?.room ?? 0, 'room');
}

/**
 * Words as they cross into Rust: 12 whole numbers below 2048, or null – the bindings would turn 65536, 0.5 or a hole in
 * the array into 0. A copy, each position read once: `every` skips holes.
 */
function indices(words: number[]): number[] | null {
  if (!Array.isArray(words) || words.length !== 12) return null;
  const copy = Array.from(words);
  return copy.every((index) => Number.isInteger(index) && index >= 0 && index < 2048) ? copy : null;
}

function valid(words: number[]): number[] {
  const checked = indices(words);
  if (!checked) throw new TempleError('InvalidWords', 'words are 12 whole numbers below 2048');
  return checked;
}

/** A whole number for Rust's 64 bits – where BigInt would throw a RangeError, a TempleError. */
function whole(value: number, what: string, signed = false): bigint {
  if (!Number.isSafeInteger(value) || (!signed && value < 0)) {
    throw new TempleError('InvalidArgument', `${what} must be a whole number${signed ? '' : ' from 0'}, not ${value}`);
  }
  return BigInt(value);
}

/**
 * The bytes a view covers, as their own ArrayBuffer – which crosses into Rust. Sliced by offset and
 * length, never via `.slice()`: a `Buffer` (Node, and the `buffer` polyfill React Native apps use)
 * aliases `slice` to `subarray`, so `.slice().buffer` would be the whole backing pool, not the view.
 */
function buffer(bytes: Uint8Array): ArrayBuffer {
  return bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) as ArrayBuffer;
}

function lift(value: BindingValue | undefined): SqlValue {
  switch (value?.tag) {
    case 'Integer': {
      const integer = value.inner!.value as bigint;
      return integer >= BigInt(Number.MIN_SAFE_INTEGER) && integer <= BigInt(Number.MAX_SAFE_INTEGER)
        ? Number(integer)
        : integer;
    }
    case 'Real':
    case 'Text':
      return value.inner!.value as number | string;
    case 'Blob':
      return new Uint8Array(value.inner!.value as ArrayBuffer);
    default:
      return null;
  }
}
