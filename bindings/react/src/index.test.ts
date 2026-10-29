// @vitest-environment happy-dom
// The hooks against a Temple in memory whose calls finish when a test lets them: what a vault
// shows while it opens, unlocks and locks – and what a lock asked for meanwhile does.

import { act, cleanup, renderHook, waitFor } from '@testing-library/react';
import { StrictMode, createElement, type ReactNode } from 'react';
import { afterEach, describe, expect, it } from 'vitest';
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
import { TempleProvider, changed, useExecute, useQuery, useVault, type VaultHandle } from './index';

afterEach(cleanup);

/** A promise the test resolves when it wants. */
function gate() {
  let open!: () => void;
  const opened = new Promise<void>((resolve) => (open = resolve));
  return { opened, open };
}

const WORDS = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];

class FakeSpace implements Space {
  locked = false;
  readOnly = false;
  rows: Row[] = [];
  queries = 0;
  failLock: TempleError | null = null;
  failQuery: TempleError | null = null;

  async execute(_sql: string, params: SqlValue[] = []) {
    this.rows.push({ body: params[0] ?? null });
    return 1;
  }
  async executeBatch() {}
  async query<T extends Row = Row>() {
    this.queries++;
    if (this.failQuery) throw this.failQuery;
    return [...this.rows] as T[];
  }
  async putObject() {
    return 1;
  }
  async writeObject(): Promise<ObjectWriter> {
    return { write: async () => {}, finish: async () => 1 };
  }
  async readObject() {
    return new Uint8Array();
  }
  async objectLength() {
    return 0;
  }
  async deleteObject() {}
  async phraseKey() {
    return new Uint8Array(32);
  }
  async changedElsewhere() {
    return false;
  }
  async isReadOnly() {
    return this.readOnly;
  }
  async changeWords() {}
  async forgetWords() {
    return false;
  }
  async keepOnlyThis() {}
  async copyToNewVault(): Promise<Vault> {
    throw new TempleError('Io', 'not in memory');
  }
  async copyInto() {}
  async lock() {
    this.locked = true;
    if (this.failLock) throw this.failLock;
  }
  async isLocked() {
    return this.locked;
  }
}

class FakeVault implements Vault {
  closed = false;
  spaces: FakeSpace[] = [];
  /** Unlocking waits for this, if set. */
  pause: Promise<void> | null = null;

  constructor(readonly temple: FakeTemple) {}

  async unlock(words: number[]) {
    await this.pause;
    if (words.join() !== WORDS.join()) return null;
    return this.open(new FakeSpace());
  }
  async unlockWithKey(key: Uint8Array) {
    return key.length === 32 ? this.open(new FakeSpace()) : null;
  }
  async salvage(words: number[]) {
    const space = await this.unlock(words);
    if (space) space.readOnly = true;
    return space;
  }
  async createSpace(words: NewWords) {
    if (!this.temple.made.has(words)) throw new TempleError('InvalidWords', 'new words come from generateWords');
    return this.open(new FakeSpace());
  }
  close() {
    this.closed = true;
    this.temple.openNow--;
  }
  open(space: FakeSpace) {
    this.spaces.push(space);
    return space;
  }
}

class FakeTemple implements Temple {
  vaults: FakeVault[] = [];
  made = new WeakSet<NewWords>();
  exists = true;
  failOpen: TempleError | null = null;
  /** Opening waits for this, if set. */
  pause: Promise<void> | null = null;
  opening = 0;
  /** How many vaults are open at once now, and at most: the web opens a file once at a time. */
  openNow = 0;
  openMost = 0;
  created: NewWords[] = [];

  async generateWords() {
    const words = { indices: WORDS };
    this.made.add(words);
    return words;
  }
  async wordsAreValid() {
    return true;
  }
  async createVault(_path: string, words: NewWords) {
    if (!this.made.has(words)) throw new TempleError('InvalidWords', 'new words come from generateWords');
    this.created.push(words);
    const vault = this.track(new FakeVault(this));
    return { vault, space: vault.open(new FakeSpace()) };
  }
  async openVault() {
    this.opening++;
    await this.pause;
    if (this.failOpen) throw this.failOpen;
    if (!this.exists) throw new TempleError('NotFound', 'no vault there');
    return this.track(new FakeVault(this));
  }
  async excludeFromBackup() {
    return false;
  }
  track(vault: FakeVault) {
    this.vaults.push(vault);
    this.openNow++;
    this.openMost = Math.max(this.openMost, this.openNow);
    return vault;
  }
}

function mount(temple: FakeTemple, strict = false, path = 'a.vault') {
  const wrapper = ({ children }: { children: ReactNode }) => {
    const provided = createElement(TempleProvider, { temple }, children);
    return strict ? createElement(StrictMode, null, provided) : provided;
  };
  return renderHook(() => useVault(path), { wrapper });
}

async function locked(result: { current: VaultHandle }) {
  await waitFor(() => expect(result.current.status).toBe('locked'));
}

describe('useVault', () => {
  it('opens the vault at the path – or finds none there, or fails', async () => {
    const temple = new FakeTemple();
    const { result } = mount(temple);
    expect(result.current.status).toBe('opening');
    await locked(result);

    const missing = new FakeTemple();
    missing.exists = false;
    const none = mount(missing, false, 'b.vault').result;
    await waitFor(() => expect(none.current.status).toBe('missing'));

    const failing = new FakeTemple();
    failing.failOpen = new TempleError('InUse', 'another tab');
    const failed = mount(failing, false, 'c.vault').result;
    await waitFor(() => expect(failed.current.status).toBe('failed'));
    expect(TempleError.is(failed.current.status === 'failed' && failed.current.error, 'InUse')).toBe(true);
  });

  it('unlocks the space the words open, and locks it', async () => {
    const temple = new FakeTemple();
    const { result } = mount(temple);
    await locked(result);
    await act(async () => expect(await result.current.unlock([12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1])).toBe(false));
    expect(result.current.status).toBe('locked');
    await act(async () => expect(await result.current.unlock(WORDS)).toBe(true));
    expect(result.current.status).toBe('open');
    const space = temple.vaults[0].spaces[0];
    await act(() => result.current.lock());
    expect(result.current.status).toBe('locked');
    expect(space.locked).toBe(true);
  });

  // From the review of 2026-10-28: a lock asked for while a space was still unlocking did
  // nothing, and the space opened afterwards – in the background, if the app was left.
  it('locks a space that opens after a lock was asked for', async () => {
    const temple = new FakeTemple();
    const { result } = mount(temple);
    await locked(result);
    const unlocking = gate();
    temple.vaults[0].pause = unlocking.opened;
    let unlocked!: Promise<boolean>;
    act(() => {
      unlocked = result.current.unlock(WORDS);
    });
    await act(() => result.current.lock());
    await act(async () => {
      unlocking.open();
      await unlocked;
    });
    expect(result.current.status).toBe('locked');
    expect(temple.vaults[0].spaces[0].locked).toBe(true);
  });

  it('shows the space locked even when locking failed', async () => {
    const temple = new FakeTemple();
    const { result } = mount(temple);
    await locked(result);
    await act(() => result.current.unlock(WORDS));
    temple.vaults[0].spaces[0].failLock = new TempleError('ChangedElsewhere', 'another device');
    await act(() => result.current.lock().catch(() => {}));
    expect(result.current.status).toBe('locked');
  });

  it('locks the open space and closes the vault when unmounted', async () => {
    const temple = new FakeTemple();
    const { result, unmount } = mount(temple);
    await locked(result);
    await act(() => result.current.unlock(WORDS));
    unmount();
    await waitFor(() => expect(temple.vaults[0].closed).toBe(true));
    expect(temple.vaults[0].spaces[0].locked).toBe(true);
  });

  it('opens a path again only once its last user let go', async () => {
    const temple = new FakeTemple();
    const opening = gate();
    temple.pause = opening.opened;
    const first = mount(temple);
    await waitFor(() => expect(temple.opening).toBe(1));
    first.unmount();
    const second = mount(temple);
    opening.open();
    await locked(second.result);
    expect(temple.vaults.length).toBe(2);
    expect(temple.vaults[0].closed).toBe(true);
    expect(temple.openMost).toBe(1);
  });

  it('opens the vault once under StrictMode, which mounts every effect twice', async () => {
    const temple = new FakeTemple();
    const { result } = mount(temple, true);
    await locked(result);
    expect(temple.openMost).toBe(1);
    await act(async () => expect(await result.current.unlock(WORDS)).toBe(true));
    expect(result.current.status).toBe('open');
  });

  it('creates a vault and spaces only under words from generateWords', async () => {
    const temple = new FakeTemple();
    temple.exists = false;
    const { result } = mount(temple);
    await waitFor(() => expect(result.current.status).toBe('missing'));
    await act(async () => {
      await expect(result.current.create({ indices: WORDS })).rejects.toThrow(TempleError);
    });
    const words = await temple.generateWords();
    await act(() => result.current.create(words));
    expect(result.current.status).toBe('open');
    expect(temple.created).toEqual([words]);
    await act(() => result.current.lock());
    await act(async () => {
      await expect(result.current.createSpace({ indices: WORDS })).rejects.toThrow(TempleError);
    });
    await act(() => result.current.createSpace(words));
    expect(result.current.status).toBe('open');
  });

  it('salvages a space read-only', async () => {
    const temple = new FakeTemple();
    const { result } = mount(temple);
    await locked(result);
    await act(async () => expect(await result.current.salvage(WORDS)).toBe(true));
    expect(result.current.status === 'open' && (await result.current.space.isReadOnly())).toBe(true);
  });
});

describe('useQuery', () => {
  it('runs again after a change, and shows a failure', async () => {
    const space = new FakeSpace();
    const temple = new FakeTemple();
    const wrapper = ({ children }: { children: ReactNode }) => createElement(TempleProvider, { temple }, children);
    const { result } = renderHook(
      () => ({ notes: useQuery<{ body: string }>(space, 'SELECT body FROM note'), execute: useExecute(space) }),
      { wrapper },
    );
    expect(result.current.notes.loading).toBe(true);
    await waitFor(() => expect(result.current.notes.loading).toBe(false));
    expect(result.current.notes.rows).toEqual([]);
    await act(() => result.current.execute('INSERT INTO note VALUES (?)', ['first']));
    await waitFor(() => expect(result.current.notes.rows).toEqual([{ body: 'first' }]));

    space.failQuery = new TempleError('Locked', 'the space has locked');
    act(() => changed(space));
    await waitFor(() => expect(TempleError.is(result.current.notes.error, 'Locked')).toBe(true));
    expect(result.current.notes.rows).toEqual([]);
  });

  it('runs once per distinct parameters, and not at all without a space', async () => {
    const space = new FakeSpace();
    const temple = new FakeTemple();
    const wrapper = ({ children }: { children: ReactNode }) => createElement(TempleProvider, { temple }, children);
    const { result, rerender } = renderHook(
      ({ on, params }: { on: Space | null; params: SqlValue[] }) => useQuery(on, 'SELECT ?', params),
      { wrapper, initialProps: { on: null as Space | null, params: [1] as SqlValue[] } },
    );
    expect(result.current).toEqual({ rows: [], error: null, loading: false });
    rerender({ on: space, params: [1] });
    await waitFor(() => expect(space.queries).toBe(1));
    // A new array with the same values keys the same; '1' is another parameter than 1.
    rerender({ on: space, params: [1] });
    rerender({ on: space, params: ['1'] });
    await waitFor(() => expect(space.queries).toBe(2));
  });
});
