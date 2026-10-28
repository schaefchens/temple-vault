// Temple's API on this platform, end to end: what every binding must do alike, in a real
// app on a real device or browser. Both demos run it; so can any app.

import { TempleError, type ErrorKind, type Space, type Temple, type Vault } from './api';

export type Check = { name: string; ok: boolean; message?: string; seconds: number };

export type Files = {
  /** A vault path for a file name: in the app's storage, or as it is on the web. */
  path(name: string): string;
  /** Deletes a file the self-test made; missing files are fine. */
  remove(path: string): Promise<void>;
};

/**
 * Runs the checks one after another, reporting each as it ends. Makes two vaults with the
 * real reserve of noise – some hundred megabytes each – and deletes them at the end.
 */
export async function selfTest(temple: Temple, files: Files, report: (check: Check) => void = () => {}): Promise<Check[]> {
  const checks: Check[] = [];
  const check = async (name: string, run: () => Promise<void>) => {
    const start = Date.now();
    let outcome: Check;
    try {
      await run();
      outcome = { name, ok: true, seconds: (Date.now() - start) / 1000 };
    } catch (error) {
      outcome = { name, ok: false, message: describe(error), seconds: (Date.now() - start) / 1000 };
    }
    checks.push(outcome);
    report(outcome);
  };

  const tag = Math.random().toString(36).slice(2, 8);
  const a = files.path(`self-test-${tag}-a.vault`);
  const b = files.path(`self-test-${tag}-b.vault`);
  let vault!: Vault;
  let space!: Space;
  let copy: Vault | null = null;
  let first!: number[];
  let second!: number[];

  await check('words: 12 indices below 2048 with their checksum', async () => {
    first = await temple.generateWords();
    assert(first.length === 12 && first.every((i) => Number.isInteger(i) && i >= 0 && i < 2048), `got ${first}`);
    assert(await temple.wordsAreValid(first), 'fresh words are valid');
    assert(!(await temple.wordsAreValid(first.slice(0, 11))), '11 words are not');
    assert(!(await temple.wordsAreValid([...first.slice(0, 11), 2048])), 'an index of 2048 is not');
    // The last word's low 4 bits are the checksum.
    assert(!(await temple.wordsAreValid([...first.slice(0, 11), first[11] ^ 1])), 'a wrong checksum is not');
  });

  await check('a new vault, its first space open', async () => {
    ({ vault, space } = await temple.createVault(a, first));
    assert(!(await space.isLocked()), 'the space is open');
  });

  await check('SQL: every kind of value comes back as it went in', async () => {
    await space.executeBatch(
      'CREATE TABLE note (id INTEGER PRIMARY KEY, body TEXT NOT NULL); CREATE TABLE kinds (i, big, r, t, b, n)',
    );
    const blob = new Uint8Array([0, 1, 127, 255]);
    const inserted = await space.execute('INSERT INTO kinds VALUES (?, ?, ?, ?, ?, ?)', [
      -42,
      2n ** 62n,
      2.5,
      'ünïcödé ✓',
      blob,
      null,
    ]);
    assert(inserted === 1, `one row inserted, got ${inserted}`);
    const [row] = await space.query('SELECT * FROM kinds');
    assert(row.i === -42, `integer: ${String(row.i)}`);
    assert(row.big === 2n ** 62n, `big integer as bigint: ${String(row.big)}`);
    assert(row.r === 2.5, `real: ${String(row.r)}`);
    assert(row.t === 'ünïcödé ✓', `text: ${String(row.t)}`);
    assert(row.b instanceof Uint8Array && equal(row.b, blob), 'blob');
    assert(row.n === null, `null: ${String(row.n)}`);
    await space.execute('INSERT INTO note (body) VALUES (?)', ['written before locking']);
  });

  await check('SQL errors are TempleError "Database"', async () => {
    await expectError(() => space.execute('SELEC nothing'), 'Database');
  });

  await check('full-text search with FTS5', async () => {
    await space.executeBatch('CREATE VIRTUAL TABLE search USING fts5(body)');
    await space.execute('INSERT INTO search VALUES (?)', ['the quick brown fox']);
    const found = await space.query("SELECT body FROM search WHERE search MATCH 'quick'");
    assert(found.length === 1 && found[0].body === 'the quick brown fox', `found ${found.length}`);
  });

  await check('objects: 1 MiB in, the same bytes out, whole and in part', async () => {
    const bytes = pattern(1 << 20);
    const id = await space.putObject(bytes);
    assert((await space.objectLength(id)) === bytes.length, 'its length');
    assert(equal(await space.readObject(id), bytes), 'the whole object');
    assert(equal(await space.readObject(id, 4000, 70_000), bytes.subarray(4000, 70_000)), 'a range across blocks');
    await space.deleteObject(id);
    await expectError(() => space.readObject(id), 'NoSuchObject');
  });

  await check('objects are refused inside a transaction', async () => {
    await space.execute('BEGIN');
    await expectError(() => space.putObject(new Uint8Array(10)), 'InTransaction');
    await space.execute('ROLLBACK');
  });

  await check('a locked space refuses every call', async () => {
    await space.lock();
    assert(await space.isLocked(), 'it says so');
    await expectError(() => space.query('SELECT 1'), 'Locked');
    await space.lock();
  });

  await check('its words open the space again; other words open none', async () => {
    assert((await vault.unlock(await temple.generateWords())) === null, 'unknown words open nothing');
    space = await opened(vault.unlock(first));
    const notes = await space.query('SELECT body FROM note');
    assert(notes.length === 1 && notes[0].body === 'written before locking', 'the note is back');
  });

  await check('one space at a time; invalid words and keys are refused', async () => {
    await expectError(() => vault.unlock(first), 'SpaceOpen');
    await expectError(() => vault.unlock(first.slice(0, 11)), 'InvalidWords');
    await expectError(() => vault.unlockWithKey(new Uint8Array(31)), 'InvalidKey');
  });

  await check('quick unlock with the phrase key', async () => {
    const key = await space.phraseKey();
    assert(key.length === 32, `32 bytes, got ${key.length}`);
    await space.lock();
    space = await opened(vault.unlockWithKey(key));
  });

  await check('a second space under other words sees nothing of the first', async () => {
    await space.lock();
    second = await temple.generateWords();
    const hidden = await vault.createSpace(second);
    const tables = await hidden.query("SELECT name FROM sqlite_master WHERE name = 'note'");
    assert(tables.length === 0, "the first space's table isn't there");
    await hidden.executeBatch("CREATE TABLE secret (body TEXT); INSERT INTO secret VALUES ('hidden')");
    await hidden.lock();
    await expectError(() => vault.createSpace(first), 'WordsInUse');
    space = await opened(vault.unlock(first));
    const secret = await space.query("SELECT name FROM sqlite_master WHERE name = 'secret'");
    assert(secret.length === 0, "the second space's table isn't there either");
  });

  await check('changed words: the old words and the old key open nothing', async () => {
    const key = await space.phraseKey();
    const changed = await temple.generateWords();
    await space.changeWords(changed);
    await space.lock();
    assert((await vault.unlock(first)) === null, 'the old words');
    assert((await vault.unlockWithKey(key)) === null, 'the old key');
    first = changed;
    space = await opened(vault.unlock(first));
  });

  await check('a vault opened again holds what was written', async () => {
    await space.lock();
    vault.close();
    vault = await temple.openVault(a);
    space = await opened(vault.unlock(first));
    assert((await space.query('SELECT body FROM note')).length === 1, 'the note');
  });

  await check('a missing vault is "NotFound"; a taken path "AlreadyExists"', async () => {
    await expectError(() => temple.openVault(files.path(`self-test-${tag}-missing.vault`)), 'NotFound');
    await expectError(() => temple.createVault(a, second), 'AlreadyExists');
  });

  await check('rebuilding: a copy into a new vault, then the second space into it', async () => {
    copy = await space.copyToNewVault(b);
    await space.lock();
    const copied = await opened(copy.unlock(first));
    assert((await copied.query('SELECT body FROM note')).length === 1, 'the note in the copy');
    await copied.lock();
    const hidden = await opened(vault.unlock(second));
    await hidden.copyInto(copy);
    await hidden.lock();
    const hiddenCopy = await opened(copy.unlock(second));
    assert((await hiddenCopy.query('SELECT body FROM secret'))[0]?.body === 'hidden', 'the secret in the copy');
    await hiddenCopy.lock();
  });

  await check('keeping only one space removes the others', async () => {
    assert(copy, 'the copy exists');
    const kept = await opened(copy.unlock(first));
    await kept.keepOnlyThis();
    await kept.lock();
    assert((await copy.unlock(second)) === null, 'the second space is gone');
    copy.close();
    copy = null;
  });

  await check('a vault opened twice: the first stops writing, or on the web the second is refused', async () => {
    space = await opened(vault.unlock(first));
    let other: Vault;
    try {
      other = await temple.openVault(a);
    } catch (error) {
      if (TempleError.is(error, 'InUse')) return;
      throw error;
    }
    try {
      await expectError(() => space.execute('INSERT INTO note (body) VALUES (?)', ['after']), 'ChangedElsewhere');
      assert(await space.changedElsewhere(), 'the space knows');
      await space.lock().catch(() => {});
    } finally {
      other.close();
    }
  });

  await check('clean up', async () => {
    await space?.lock().catch(() => {});
    vault?.close();
    copy?.close();
    await files.remove(a);
    await files.remove(b);
  });

  return checks;
}

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

async function opened(space: Promise<Space | null>): Promise<Space> {
  const open = await space;
  assert(open, 'the words open a space');
  return open;
}

async function expectError(call: () => Promise<unknown>, kind: ErrorKind) {
  try {
    await call();
  } catch (error) {
    if (TempleError.is(error, kind)) return;
    throw new Error(`expected ${kind}, got ${describe(error)}`);
  }
  throw new Error(`expected ${kind}, but it worked`);
}

function describe(error: unknown): string {
  if (TempleError.is(error)) return `${error.kind}: ${error.message}`;
  return error instanceof Error ? error.message : String(error);
}

function equal(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((byte, i) => byte === b[i]);
}

/** Bytes that differ from block to block, without a random source (Hermes has none). */
function pattern(length: number): Uint8Array {
  const bytes = new Uint8Array(length);
  let x = 0x9e3779b9;
  for (let i = 0; i < length; i++) {
    x ^= x << 13;
    x ^= x >>> 17;
    x ^= x << 5;
    bytes[i] = x & 0xff;
  }
  return bytes;
}
