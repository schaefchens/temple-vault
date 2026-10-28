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
      const space = await run(() => this.inner.unlock(words));
      return space ? new WrappedSpace(space) : null;
    }

    async unlockWithKey(key: Uint8Array) {
      const space = await run(() => this.inner.unlockWithKey(buffer(key)));
      return space ? new WrappedSpace(space) : null;
    }

    async createSpace(words: number[], options?: { room?: number }) {
      return new WrappedSpace(await run(() => this.inner.createSpace(words, room(options))));
    }

    close() {
      this.inner.uniffiDestroy?.();
    }
  }

  class WrappedSpace implements Space {
    #inner: BindingSpace | null;

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
      const to = end === undefined ? END : BigInt(end);
      return new Uint8Array(await run(() => this.#space.readObject(BigInt(id), BigInt(start), to)));
    }

    async objectLength(id: number) {
      return Number(await run(() => this.#space.objectLen(BigInt(id))));
    }

    deleteObject(id: number) {
      return run(() => this.#space.deleteObject(BigInt(id)));
    }

    async phraseKey() {
      return new Uint8Array(await run(() => this.#space.phraseKey()));
    }

    changedElsewhere() {
      return run(() => this.#space.changedElsewhere());
    }

    changeWords(words: number[]) {
      return run(() => this.#space.changeWords(words));
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

    /** Even when locking fails – another device opened the vault – the space is closed. */
    async lock() {
      const space = this.#inner;
      if (!space) return;
      try {
        await run(() => space.lock());
      } finally {
        this.#inner = null;
        space.uniffiDestroy?.();
      }
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
      return binding.wordsAreValid(words);
    },
    async createVault(path: string, words: number[], options?: { room?: number }) {
      const { vault, space } = await run(() => binding.createVault(path, words, room(options)));
      return { vault: new WrappedVault(vault), space: new WrappedSpace(space) };
    },
    async openVault(path: string) {
      return new WrappedVault(await run(() => binding.openVault(path)));
    },
  };
}

function room(options?: { room?: number }): bigint {
  return BigInt(options?.room ?? 0);
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
