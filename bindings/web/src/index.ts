// Temple for the web: vaults are files of the origin private file system, and Temple runs in
// a worker of its own – only a worker can read and write such a file in place. `temple`
// starts it with its first call. A vault opens in one tab at a time; another gets "InUse".

import {
  TempleError,
  type NewWords,
  type ObjectWriter,
  type Row,
  type Space,
  type SqlValue,
  type Temple,
  type Vault,
} from '@schaefchens/temple';
import type { Call, Close, Fatal, Ref, Reply } from './protocol';

export * from '@schaefchens/temple';

type Pending = { resolve(value: unknown): void; reject(error: unknown): void };

/** The worker, started on demand – and again after a failure, which loses what it held. */
class Remote {
  #worker: Worker | null = null;
  #pending = new Map<number, Pending>();
  #next = 1;
  generation = 0;

  call<T>(target: number, method: string, args: unknown[], generation = this.generation): Promise<T> {
    if (generation !== this.generation) {
      return Promise.reject(new TempleError('Locked', 'Temple restarted: open the vault again'));
    }
    const worker = this.#start();
    const id = this.#next++;
    return new Promise<T>((resolve, reject) => {
      this.#pending.set(id, { resolve: resolve as (value: unknown) => void, reject });
      worker.postMessage({ id, target, method, args } satisfies Call);
    });
  }

  close(handle: number, generation: number) {
    if (generation === this.generation) this.#worker?.postMessage({ close: handle } satisfies Close);
  }

  #start(): Worker {
    if (this.#worker) return this.#worker;
    const worker = new Worker(new URL('./worker.ts', import.meta.url), { type: 'module' });
    worker.onmessage = ({ data }: MessageEvent<Reply | Fatal>) => {
      if ('fatal' in data) return this.#stop(new TempleError('Io', `Temple failed: ${data.fatal}`));
      const pending = this.#pending.get(data.id);
      this.#pending.delete(data.id);
      if ('error' in data) pending?.reject(new TempleError(data.error.kind, data.error.message));
      else pending?.resolve(data.value);
    };
    worker.onerror = (event) => this.#stop(new TempleError('Io', `Temple's worker failed: ${event.message}`));
    this.#worker = worker;
    return worker;
  }

  #stop(error: TempleError) {
    this.#worker?.terminate();
    this.#worker = null;
    this.generation++;
    for (const { reject } of this.#pending.values()) reject(error);
    this.#pending.clear();
  }
}

const remote = new Remote();

/** What the page lets go of without closing or locking it, the worker closes or locks once it's collected – as React Native's garbage collection does. */
const vaultsLetGo = new FinalizationRegistry<{ handle: number; generation: number }>(({ handle, generation }) =>
  remote.close(handle, generation),
);
const spacesLetGo = new FinalizationRegistry<{ handle: number; generation: number }>(({ handle, generation }) => {
  remote.call(handle, 'lock', [], generation).catch(() => {});
});

/** Words and object writers the page let go of: the worker forgets them, and Rust wipes the words. */
const othersLetGo = new FinalizationRegistry<{ handle: number; generation: number }>(({ handle, generation }) =>
  remote.close(handle, generation),
);

/** New words living in the worker: the page holds their number, and their indices to show. */
class RemoteNewWords implements NewWords {
  readonly generation = remote.generation;

  constructor(
    readonly handle: number,
    readonly indices: number[],
  ) {
    othersLetGo.register(this, { handle, generation: this.generation });
  }
}

/** New words as the worker takes them: only those it made. */
function fresh(words: NewWords): Ref {
  if (!(words instanceof RemoteNewWords) || words.generation !== remote.generation) {
    throw new TempleError('InvalidWords', 'new words come from generateWords, never from the user');
  }
  return { newWords: words.handle };
}

/** Options naming new words, as the worker takes them. */
function withWords(options?: { words?: NewWords }) {
  return { words: options?.words && fresh(options.words) };
}

class RemoteVault implements Vault {
  readonly generation = remote.generation;

  constructor(readonly handle: number) {
    vaultsLetGo.register(this, { handle, generation: this.generation }, this);
  }

  #call<T>(method: string, ...args: unknown[]) {
    return remote.call<T>(this.handle, method, args, this.generation);
  }

  async unlock(words: number[]) {
    return space(await this.#call<Ref | null>('unlock', words));
  }

  async unlockWithKey(key: Uint8Array) {
    return space(await this.#call<Ref | null>('unlockWithKey', key));
  }

  async salvage(words: number[]) {
    return space(await this.#call<Ref | null>('salvage', words));
  }

  async createSpace(words: NewWords, options?: { room?: number }) {
    return space(await this.#call<Ref>('createSpace', fresh(words), options))!;
  }

  close() {
    vaultsLetGo.unregister(this);
    remote.close(this.handle, this.generation);
  }
}

class RemoteSpace implements Space {
  readonly generation = remote.generation;
  #locked = false;
  #locking: Promise<void> | null = null;

  constructor(readonly handle: number) {
    spacesLetGo.register(this, { handle, generation: this.generation }, this);
  }

  #call<T>(method: string, ...args: unknown[]) {
    if (this.#locked) return Promise.reject(new TempleError('Locked', 'the space has locked'));
    return remote.call<T>(this.handle, method, args, this.generation);
  }

  execute(sql: string, params?: SqlValue[]) {
    return this.#call<number>('execute', sql, params);
  }

  executeBatch(sql: string) {
    return this.#call<void>('executeBatch', sql);
  }

  query<T extends Row = Row>(sql: string, params?: SqlValue[]) {
    return this.#call<T[]>('query', sql, params);
  }

  putObject(content: Uint8Array) {
    return this.#call<number>('putObject', content);
  }

  async writeObject(): Promise<ObjectWriter> {
    return new RemoteObjectWriter(((await this.#call<Ref>('writeObject')) as { writer: number }).writer);
  }

  readObject(id: number, start?: number, end?: number) {
    return this.#call<Uint8Array>('readObject', id, start, end);
  }

  objectLength(id: number) {
    return this.#call<number>('objectLength', id);
  }

  deleteObject(id: number) {
    return this.#call<void>('deleteObject', id);
  }

  phraseKey() {
    return this.#call<Uint8Array>('phraseKey');
  }

  changedElsewhere() {
    return this.#call<boolean>('changedElsewhere');
  }

  isReadOnly() {
    return this.#call<boolean>('isReadOnly');
  }

  async changeWords(words: NewWords) {
    return this.#call<void>('changeWords', fresh(words));
  }

  forgetWords(words: number[]) {
    return this.#call<boolean>('forgetWords', words);
  }

  keepOnlyThis() {
    return this.#call<void>('keepOnlyThis');
  }

  async copyToNewVault(path: string, options?: { words?: NewWords }) {
    return vault(await this.#call<Ref>('copyToNewVault', path, withWords(options)));
  }

  async copyInto(target: Vault, options?: { words?: NewWords }) {
    // A vault from before the worker restarted: its number may name another vault now.
    if (!(target instanceof RemoteVault) || target.generation !== remote.generation) {
      throw new TempleError('NotFound', 'not a vault of this Temple – open it again');
    }
    return this.#call<void>('copyInto', { vault: target.handle }, withWords(options));
  }

  /** Even when locking fails – another device opened the vault – the space is closed. Calls while it locks share that lock. */
  lock() {
    if (!this.#locked && !this.#locking) {
      spacesLetGo.unregister(this);
      this.#locking = this.#call<void>('lock').finally(() => {
        this.#locked = true;
        this.#locking = null;
      });
    }
    return this.#locking ?? Promise.resolve();
  }

  async isLocked() {
    return this.#locked;
  }
}

class RemoteObjectWriter implements ObjectWriter {
  readonly generation = remote.generation;
  #finished = false;

  constructor(readonly handle: number) {
    othersLetGo.register(this, { handle, generation: this.generation }, this);
  }

  #call<T>(method: string, ...args: unknown[]) {
    if (this.#finished) return Promise.reject(new TempleError('NoSuchObject', 'the object is finished'));
    return remote.call<T>(this.handle, method, args, this.generation);
  }

  write(bytes: Uint8Array) {
    return this.#call<void>('write', bytes);
  }

  finish() {
    const finished = this.#call<number>('finish');
    this.#finished = true;
    othersLetGo.unregister(this);
    return finished;
  }
}

function vault(ref: Ref): Vault {
  return new RemoteVault((ref as { vault: number }).vault);
}

function space(ref: Ref | null): Space | null {
  return ref ? new RemoteSpace((ref as { space: number }).space) : null;
}

export const temple: Temple = {
  async generateWords() {
    const { newWords, indices } = await remote.call<{ newWords: number; indices: number[] }>(0, 'generateWords', []);
    return new RemoteNewWords(newWords, indices);
  },
  wordsAreValid: (words) => remote.call<boolean>(0, 'wordsAreValid', [words]),
  async createVault(path, words, options) {
    const created = await remote.call<{ vault: Ref; space: Ref }>(0, 'createVault', [path, fresh(words), options]);
    return { vault: vault(created.vault), space: space(created.space)! };
  },
  excludeFromBackup: (path) => remote.call<boolean>(0, 'excludeFromBackup', [path]),
  /** A page that just reloaded may find its predecessor's worker still holding the file: two seconds' grace. */
  async openVault(path, options) {
    for (let attempt = 0; ; attempt++) {
      try {
        return vault(await remote.call<Ref>(0, 'openVault', [path, options]));
      } catch (error) {
        if (!TempleError.is(error, 'InUse') || attempt === 10) throw error;
        await new Promise((resolve) => setTimeout(resolve, 200));
      }
    }
  },
};
