// Messages between the page and Temple's worker. Vaults and spaces stay in the worker; the
// page refers to them by number, 0 being the Temple itself.

import type { ErrorKind } from '@schaefchens/temple';

export type Call = { id: number; target: number; method: string; args: unknown[] };
export type Close = { close: number };
export type Reply = { id: number; value: unknown } | { id: number; error: { kind: ErrorKind; message: string } };
/** The worker can't go on: a panic poisons a wasm instance. */
export type Fatal = { fatal: string };

/** A vault, a space, an object being written or new words in a message – new words with their indices. */
export type Ref = { vault: number } | { space: number } | { writer: number } | { newWords: number; indices?: number[] };
