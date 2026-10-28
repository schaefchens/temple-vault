// React hooks for Temple: the vault at a path and its open space, queries that run again
// after writes. They work with either platform's Temple, given through `TempleProvider`.

import { createContext, createElement, useCallback, useContext, useEffect, useRef, useState, type ReactNode } from 'react';
import { TempleError, type Row, type Space, type SqlValue, type Temple, type Vault } from '@schaefchens/temple';

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
  /** A new vault at the path with its first space, which opens. */
  create(words: number[], options?: { room?: number }): Promise<void>;
  /** Opens the space the words open; false if they open none. */
  unlock(words: number[]): Promise<boolean>;
  /** Quick unlock with a kept phrase key; false if it opens nothing. */
  unlockWithKey(key: Uint8Array): Promise<boolean>;
  /** A new space for words that open none, which opens – once the user confirmed them. */
  createSpace(words: number[], options?: { room?: number }): Promise<void>;
  /** Locks the open space. */
  lock(): Promise<void>;
};

/** What one mount of `useVault` holds. Once done, whatever arrives late is let go at once. */
type Session = { state: VaultState; done: boolean; show(next: VaultState): void };

// Per path, when its last user will have let go: on the web a file opens once at a time,
// and StrictMode mounts every effect twice.
const releases = new Map<string, Promise<void>>();

/**
 * The vault at `path` and its open space, if any: a vault opens one space at a time. The
 * space locks and the vault closes when the component unmounts or `path` changes.
 */
export function useVault(path: string): VaultHandle {
  const temple = useTemple();
  const [state, setState] = useState<VaultState>({ status: 'opening' });
  const session = useRef<Session>({ state, done: false, show: setState });

  useEffect(() => {
    let release!: () => void;
    const released = new Promise<void>((resolve) => (release = resolve));
    const previous = releases.get(path) ?? Promise.resolve();
    releases.set(path, released);

    const current: Session = {
      state: { status: 'opening' },
      done: false,
      show(next) {
        if (current.done) {
          void letGo(next);
          return;
        }
        current.state = next;
        setState(next);
      },
    };
    session.current = current;
    setState(current.state);

    const opened = previous
      .then(() => (current.done ? null : temple.openVault(path)))
      .then(
        (vault) => vault && current.show({ status: 'locked', vault }),
        (error) => current.show(TempleError.is(error, 'NotFound') ? { status: 'missing' } : { status: 'failed', error }),
      );
    return () => {
      current.done = true;
      void opened.then(() => letGo(current.state)).finally(() => {
        if (releases.get(path) === released) releases.delete(path);
        release();
      });
    };
  }, [temple, path]);

  const vault = () => {
    const state = session.current.state;
    if (state.status === 'locked' || state.status === 'open') return state.vault;
    throw new TempleError('NotFound', `no vault is open at ${path}`);
  };
  const create = useCallback(
    async (words: number[], options?: { room?: number }) => {
      const mount = session.current;
      const { vault, space } = await temple.createVault(path, words, options);
      mount.show({ status: 'open', vault, space });
    },
    [temple, path],
  );
  const unlock = useCallback(async (words: number[]) => {
    const mount = session.current;
    const opened = vault();
    const space = await opened.unlock(words);
    if (space) mount.show({ status: 'open', vault: opened, space });
    return space !== null;
  }, []);
  const unlockWithKey = useCallback(async (key: Uint8Array) => {
    const mount = session.current;
    const opened = vault();
    const space = await opened.unlockWithKey(key);
    if (space) mount.show({ status: 'open', vault: opened, space });
    return space !== null;
  }, []);
  const createSpace = useCallback(async (words: number[], options?: { room?: number }) => {
    const mount = session.current;
    const opened = vault();
    const space = await opened.createSpace(words, options);
    mount.show({ status: 'open', vault: opened, space });
  }, []);
  const lock = useCallback(async () => {
    const mount = session.current;
    const state = mount.state;
    if (state.status !== 'open') return;
    await state.space.lock();
    mount.show({ status: 'locked', vault: state.vault });
  }, []);

  return { ...state, create, unlock, unlockWithKey, createSpace, lock };
}

/** Locks what a state holds open and closes its vault. */
async function letGo(state: VaultState) {
  if (state.status === 'open') await state.space.lock().catch(() => {});
  if (state.status === 'open' || state.status === 'locked') state.vault.close();
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
  const key = params.map(paramKey).join('\u0000');
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

function paramKey(value: SqlValue): string {
  if (value instanceof Uint8Array) return `b:${Array.from(value).join(',')}`;
  return `${typeof value}:${String(value)}`;
}
