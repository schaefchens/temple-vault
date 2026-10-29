// Temple's worker: it hosts the wasm module – only a worker can open the sync access
// handles a vault's file needs – and runs the page's calls on its vaults and spaces.

import { TempleError, wrap, type NewWords, type ObjectWriter, type Space, type Vault } from '@schaefchens/temple';
import { uniffiInitAsync } from './generated/index';
import * as binding from './generated/temple';
import type { Call, Close, Fatal, Reply } from './protocol';

const temple = uniffiInitAsync(new URL('./generated/temple_ffi.wasm', import.meta.url)).then(() => wrap(binding));
const vaults = new Map<number, Vault>();
const spaces = new Map<number, Space>();
const writers = new Map<number, ObjectWriter>();
const newWords = new Map<number, NewWords>();
const known = new Map<Vault | Space | ObjectWriter | NewWords, number>();
let next = 1;

self.onmessage = async ({ data }: MessageEvent<Call | Close>) => {
  if ('close' in data) {
    vaults.get(data.close)?.close();
    forget(data.close);
    return;
  }
  const { id, target, method, args } = data;
  try {
    const receiver = target === 0 ? await temple : (vaults.get(target) ?? spaces.get(target) ?? writers.get(target));
    if (!receiver) throw new TempleError('Locked', 'this vault or space is gone');
    const call = (receiver as unknown as Record<string, (...args: unknown[]) => unknown>)[method];
    // A space is closed once it has locked, even when locking failed; a writer once it finished.
    const value = await Promise.resolve(call.apply(receiver, args.map(resolve))).finally(() => {
      if (method === 'lock' || method === 'finish') forget(target);
    });
    const transfer = value instanceof Uint8Array ? [value.buffer as ArrayBuffer] : [];
    post({ id, value: refer(value) }, transfer);
  } catch (error) {
    if (error instanceof WebAssembly.RuntimeError) {
      // A Rust panic, or a broken module: the instance can't be trusted any more.
      post({ fatal: String(error) });
      self.close();
    } else if (TempleError.is(error)) {
      post({ id, error: { kind: error.kind, message: error.message } });
    } else {
      // Something the page sent wrong, before it reached Temple: an error of its own call.
      post({ id, error: { kind: 'InvalidArgument', message: error instanceof Error ? error.message : String(error) } });
    }
  }
};

function post(message: Reply | Fatal, transfer: Transferable[] = []) {
  self.postMessage(message, { transfer });
}

/** Vaults, spaces, writers and new words as numbers – the page holds them by those. */
function refer(value: unknown): unknown {
  if (isTemple(value, 'unlock')) return { vault: remember(value as Vault, vaults) };
  if (isTemple(value, 'execute')) return { space: remember(value as Space, spaces) };
  if (isTemple(value, 'finish')) return { writer: remember(value as ObjectWriter, writers) };
  if (value && typeof value === 'object' && 'indices' in value && Array.isArray(value.indices)) {
    return { newWords: remember(value as NewWords, newWords), indices: value.indices };
  }
  if (value && typeof value === 'object' && 'vault' in value && 'space' in value) {
    return { vault: refer(value.vault), space: refer(value.space) };
  }
  return value;
}

/** What the page sent, with its numbers for vaults and new words – in options too – made the objects again. */
function resolve(arg: unknown): unknown {
  if (!arg || typeof arg !== 'object' || ArrayBuffer.isView(arg) || Array.isArray(arg)) return arg;
  if ('vault' in arg && typeof arg.vault === 'number') {
    const vault = vaults.get(arg.vault);
    if (!vault) throw new TempleError('NotFound', 'that vault has closed');
    return vault;
  }
  if ('newWords' in arg && typeof arg.newWords === 'number') {
    const words = newWords.get(arg.newWords);
    if (!words) throw new TempleError('InvalidWords', 'those new words are gone – generate others');
    return words;
  }
  return Object.fromEntries(Object.entries(arg).map(([key, value]) => [key, resolve(value)]));
}

function remember<T extends Vault | Space | ObjectWriter | NewWords>(object: T, map: Map<number, T>): number {
  const known_ = known.get(object);
  if (known_ !== undefined) return known_;
  const handle = next++;
  map.set(handle, object);
  known.set(object, handle);
  return handle;
}

function forget(handle: number) {
  const object = vaults.get(handle) ?? spaces.get(handle) ?? writers.get(handle) ?? newWords.get(handle);
  if (object) known.delete(object);
  vaults.delete(handle);
  spaces.delete(handle);
  writers.delete(handle);
  newWords.delete(handle);
}

function isTemple(value: unknown, method: string): boolean {
  return typeof value === 'object' && value !== null && typeof (value as Record<string, unknown>)[method] === 'function';
}
