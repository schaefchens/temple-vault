// The API of api.ts made from the bindings uniffi-bindgen-react-native generates – the same
// module on React Native and in the web's worker. Plain JS values in, plain JS values out:
// numbers for ids and sizes, Uint8Array for bytes, null for nothing, TempleError for errors.

import { TempleError, type ErrorKind, type Row, type Space, type SqlValue, type Temple, type Vault } from './api';

/** What `wrap` needs of the generated module; `import * as binding from './generated/temple'`. */
export interface Binding {
  generateWords(): number[];
  wordsAreValid(words: number[]): boolean;
  createVault(path: string, words: number[], room: bigint): Promise<{ vault: BindingVault; space: BindingSpace }>;
  openVault(path: string): Promise<BindingVault>;
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

interface BindingVault extends Destroyable {
  unlock(words: number[]): Promise<BindingSpace | undefined>;
  unlockWithKey(key: ArrayBuffer): Promise<BindingSpace | undefined>;
  createSpace(words: number[], room: bigint): Promise<BindingSpace>;
}

interface BindingSpace extends Destroyable {
  execute(sql: string, params: unknown[]): Promise<bigint>;
  executeBatch(sql: string): Promise<void>;
  query(sql: string, params: unknown[]): Promise<{ columns: string[]; rows: BindingValue[][] }>;
  putObject(content: ArrayBuffer): Promise<bigint>;
  readObject(id: bigint, start: bigint, end: bigint): Promise<ArrayBuffer>;
  objectLen(id: bigint): Promise<bigint>;
  deleteObject(id: bigint): Promise<void>;
  phraseKey(): Promise<ArrayBuffer>;
  changedElsewhere(): Promise<boolean>;
  changeWords(words: number[]): Promise<void>;
  forgetWords(words: number[]): Promise<boolean>;
  keepOnlyThis(): Promise<void>;
  copyToNewVault(path: string): Promise<BindingVault>;
  copyInto(vault: BindingVault): Promise<void>;
  lock(): Promise<void>;
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
    if (typeof value === 'bigint') return new Value.Integer({ value });
    if (typeof value === 'number') {
      return Number.isSafeInteger(value) ? new Value.Integer({ value: BigInt(value) }) : new Value.Real({ value });
    }
    if (typeof value === 'string') return new Value.Text({ value });
    if (value instanceof Uint8Array) return new Value.Blob({ value: buffer(value) });
    throw new TempleError('Database', `SQLite can't store a ${typeof value}`);
  };

  class WrappedVault implements Vault {
    constructor(readonly inner: BindingVault) {}

    async unlock(words: number[]) {
      const space = await run(() => this.inner.unlock(valid(words)));
      return space ? new WrappedSpace(space) : null;
    }

    async unlockWithKey(key: Uint8Array) {
      const space = await run(() => this.inner.unlockWithKey(buffer(key)));
      return space ? new WrappedSpace(space) : null;
    }

    async createSpace(words: number[], options?: { room?: number }) {
      return new WrappedSpace(await run(() => this.inner.createSpace(valid(words), room(options))));
    }

    close() {
      this.inner.uniffiDestroy?.();
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
      return Number(await run(() => this.#space.execute(sql, params.map(lower))));
    }

    executeBatch(sql: string) {
      return run(() => this.#space.executeBatch(sql));
    }

    async query<T extends Row = Row>(sql: string, params: SqlValue[] = []) {
      const { columns, rows } = await run(() => this.#space.query(sql, params.map(lower)));
      return rows.map((row) => Object.fromEntries(columns.map((column, i) => [column, lift(row[i])])) as T);
    }

    async putObject(content: Uint8Array) {
      return Number(await run(() => this.#space.putObject(buffer(content))));
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

    async changeWords(words: number[]) {
      return run(() => this.#space.changeWords(valid(words)));
    }

    async forgetWords(words: number[]) {
      return run(() => this.#space.forgetWords(valid(words)));
    }

    keepOnlyThis() {
      return run(() => this.#space.keepOnlyThis());
    }

    async copyToNewVault(path: string) {
      return new WrappedVault(await run(() => this.#space.copyToNewVault(path)));
    }

    async copyInto(vault: Vault) {
      if (!(vault instanceof WrappedVault)) throw new TempleError('NotFound', 'not a vault of this Temple');
      return run(() => this.#space.copyInto(vault.inner));
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

  return {
    async generateWords() {
      return binding.generateWords();
    },
    async wordsAreValid(words: number[]) {
      return indices(words) !== null && binding.wordsAreValid(words);
    },
    async createVault(path: string, words: number[], options?: { room?: number }) {
      const { vault, space } = await run(() => binding.createVault(path, valid(words), room(options)));
      return { vault: new WrappedVault(vault), space: new WrappedSpace(space) };
    },
    async openVault(path: string) {
      return new WrappedVault(await run(() => binding.openVault(path)));
    },
  };
}

function room(options?: { room?: number }): bigint {
  return whole(options?.room ?? 0, 'room');
}

/** Words as they cross into Rust: 12 whole numbers below 2048, or null – the bindings would turn 65536 or 0.5 into 0. */
function indices(words: number[]): number[] | null {
  const valid = Array.isArray(words) && words.length === 12;
  return valid && words.every((index) => Number.isInteger(index) && index >= 0 && index < 2048) ? words : null;
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

/** The bytes a view covers, as their own ArrayBuffer – which crosses into Rust. */
function buffer(bytes: Uint8Array): ArrayBuffer {
  return bytes.slice().buffer as ArrayBuffer;
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
