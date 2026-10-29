// React hooks for Temple: the vault at a path and its open space, queries that run again
// after writes. They work with either platform's Temple, given through `TempleProvider`.

import { createContext, createElement, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from 'react';
import { TempleError, type NewWords, type Row, type Space, type SqlValue, type Temple, type Vault } from '@schaefchens/temple';

const TempleContext = createContext<Temple | null>(null);

/** Makes a platform's Temple – from `@schaefchens/temple-react-native` or `-web` – available below. */
export function TempleProvider({ temple, children }: { temple: Temple; children?: ReactNode }) {
  return createElement(TempleContext.Provider, { value: temple }, children);
}

export function useTemple(): Temple {
  const temple = useContext(TempleContext);
  if (!temple) throw new Error('Temple hooks need a TempleProvider');
  return temple;
}

export type VaultState =
  | { status: 'opening' }
  /** No vault at the path yet. */
  | { status: 'missing' }
  | { status: 'locked'; vault: Vault }
  | { status: 'open'; vault: Vault; space: Space }
  | { status: 'failed'; error: unknown };

export type VaultHandle = VaultState & {
  /** A new vault at the path with its first space, which opens – under words from `generateWords`. */
  create(words: NewWords, options?: { room?: number }): Promise<void>;
  /** Opens the space the words open; false if they open none. */
  unlock(words: number[]): Promise<boolean>;
  /** Quick unlock with a kept phrase key; false if it opens nothing. */
  unlockWithKey(key: Uint8Array): Promise<boolean>;
  /** Opens what can still be read of a damaged space, read-only; false if the words open none. */
  salvage(words: number[]): Promise<boolean>;
  /** A new space under words from `generateWords`, which opens – for words that opened none, once the user confirmed. */
  createSpace(words: NewWords, options?: { room?: number }): Promise<void>;
  /** Locks the open space, and any space a running call still opens; the state shows it locked even when locking failed. */
  lock(): Promise<void>;
};

/**
 * What one mount of `useVault` holds. `locks` counts lock requests: a space a call opens after
 * one locks at once. Once done, whatever arrives late is let go – the call that brought it
 * settles once it has – and the path is released only when the mount's running calls have
 * settled.
 */
type Session = {
  state: VaultState;
  done: boolean;
  locks: number;
  running: Set<Promise<unknown>>;
  show(next: VaultState): Promise<void>;
};

// Per path, when its last user will have let go: on the web a file opens once at a time,
// and StrictMode mounts every effect twice.
const releases = new Map<string, Promise<void>>();

/**
 * The vault at `path` and its open space, if any: a vault opens one space at a time. The
 * space locks and the vault closes when the component unmounts or `path` changes – and a
 * space that opens after `lock` was called locks at once. `readOnly`: nothing is written to
 * the vault, and its spaces open read-only.
 */
export function useVault(path: string, options?: { readOnly?: boolean }): VaultHandle {
  const readOnly = options?.readOnly ?? false;
  const temple = useTemple();
  const [state, setState] = useState<VaultState>({ status: 'opening' });
  const session = useRef<Session>({ state, done: false, locks: 0, running: new Set(), show: async (next) => setState(next) });

  useEffect(() => {
    let release!: () => void;
    const released = new Promise<void>((resolve) => (release = resolve));
    const previous = releases.get(path) ?? Promise.resolve();
    releases.set(path, released);

    const current: Session = {
      state: { status: 'opening' },
      done: false,
      locks: 0,
      running: new Set(),
      async show(next) {
        if (current.done) return letGo(next);
        current.state = next;
        setState(next);
      },
    };
    session.current = current;
    setState(current.state);

    const opened = previous
      .then(() => (current.done ? null : temple.openVault(path, { readOnly })))
      .then(
        (vault) => vault && current.show({ status: 'locked', vault }),
        (error) => current.show(TempleError.is(error, 'NotFound') ? { status: 'missing' } : { status: 'failed', error }),
      );
    return () => {
      current.done = true;
      void Promise.allSettled([opened, ...current.running])
        .then(() => letGo(current.state))
        .finally(() => {
          if (releases.get(path) === released) releases.delete(path);
          release();
        });
    };
  }, [temple, path, readOnly]);

  /** Runs one of this mount's calls, so that its end can wait for it. */
  const run = <T>(call: (mount: Session) => Promise<T>): Promise<T> => {
    const mount = session.current;
    const running = call(mount);
    mount.running.add(running);
    void running.finally(() => mount.running.delete(running)).catch(() => {});
    return running;
  };
  const vault = () => {
    const state = session.current.state;
    if (state.status === 'locked' || state.status === 'open') return state.vault;
    throw new TempleError('NotFound', `no vault is open at ${path}`);
  };
  const create = useCallback(
    (words: NewWords, options?: { room?: number }) =>
      run(async (mount) => {
        const locks = mount.locks;
        const { vault, space } = await temple.createVault(path, words, options);
        await show(mount, locks, vault, space);
      }),
    [temple, path],
  );
  const unlock = useCallback(
    (words: number[]) =>
      run(async (mount) => {
        const [locks, opened] = [mount.locks, vault()];
        const space = await opened.unlock(words);
        if (space) await show(mount, locks, opened, space);
        return space !== null;
      }),
    [],
  );
  const unlockWithKey = useCallback(
    (key: Uint8Array) =>
      run(async (mount) => {
        const [locks, opened] = [mount.locks, vault()];
        const space = await opened.unlockWithKey(key);
        if (space) await show(mount, locks, opened, space);
        return space !== null;
      }),
    [],
  );
  const salvage = useCallback(
    (words: number[]) =>
      run(async (mount) => {
        const [locks, opened] = [mount.locks, vault()];
        const space = await opened.salvage(words);
        if (space) await show(mount, locks, opened, space);
        return space !== null;
      }),
    [],
  );
  const createSpace = useCallback(
    (words: NewWords, options?: { room?: number }) =>
      run(async (mount) => {
        const [locks, opened] = [mount.locks, vault()];
        await show(mount, locks, opened, await opened.createSpace(words, options));
      }),
    [],
  );
  const lock = useCallback(
    () =>
      run(async (mount) => {
        mount.locks++;
        const state = mount.state;
        if (state.status !== 'open') return;
        try {
          await state.space.lock();
        } finally {
          // Closed even when locking failed, as when another device took over.
          await mount.show({ status: 'locked', vault: state.vault });
        }
      }),
    [],
  );

  return { ...state, create, unlock, unlockWithKey, salvage, createSpace, lock };
}

/** A space a call opened: shown – or, if a lock was asked for meanwhile, locked at once. */
async function show(mount: Session, locks: number, vault: Vault, space: Space) {
  if (mount.locks === locks) return mount.show({ status: 'open', vault, space });
  await space.lock().catch(() => {});
  await mount.show({ status: 'locked', vault });
}

const closed = new WeakSet<Vault>();

/** Locks what a state holds open and closes its vault, once. */
async function letGo(state: VaultState) {
  if (state.status === 'open') await state.space.lock().catch(() => {});
  if ((state.status === 'open' || state.status === 'locked') && !closed.has(state.vault)) {
    closed.add(state.vault);
    state.vault.close();
  }
}

const listeners = new WeakMap<Space, Set<() => void>>();

/** Tells every `useQuery` on `space` that it changed, so they run again. */
export function changed(space: Space) {
  listeners.get(space)?.forEach((listener) => listener());
}

/** Runs a statement on `space`, then `changed(space)`; returns how many rows it changed. */
export function useExecute(space: Space | null) {
  return useCallback(
    async (sql: string, params?: SqlValue[]) => {
      if (!space) throw new TempleError('Locked', 'no space is open');
      const count = await space.execute(sql, params);
      changed(space);
      return count;
    },
    [space],
  );
}

export type QueryState<T> = { rows: T[]; error: unknown; loading: boolean };

/** The rows of a query on `space`, run again whenever `changed(space)`. */
export function useQuery<T extends Row = Row>(space: Space | null, sql: string, params: SqlValue[] = []): QueryState<T> {
  const [state, setState] = useState<QueryState<T>>({ rows: [], error: null, loading: space !== null });
  const key = JSON.stringify(params.map(paramKey));
  useEffect(() => {
    if (!space) {
      setState({ rows: [], error: null, loading: false });
      return;
    }
    let active = true;
    let latest = 0;
    const run = () => {
      const run = ++latest;
      space.query<T>(sql, params).then(
        (rows) => active && run === latest && setState({ rows, error: null, loading: false }),
        (error) => active && run === latest && setState({ rows: [], error, loading: false }),
      );
    };
    const set = listeners.get(space) ?? new Set();
    listeners.set(space, set);
    set.add(run);
    run();
    return () => {
      active = false;
      set.delete(run);
    };
  }, [space, sql, key]);
  return state;
}

/** A parameter as part of the query's key: its type and value, which no other parameter list shares. */
function paramKey(value: SqlValue): [string, string | number[] | null] {
  if (value instanceof Uint8Array) return ['blob', Array.from(value)];
  return [typeof value, value === null ? null : String(value)];
}
