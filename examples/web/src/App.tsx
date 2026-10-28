// Notes on Temple in the browser: one vault in the origin private file system, spaces opened
// by 12 words, notes in each space's database with full-text search, files as objects.
// Hiding the page locks the space. `?self-test` runs Temple's self-test here instead.

import { TempleProvider, useExecute, useQuery, useVault, type VaultHandle } from '@schaefchens/temple-react';
import { TempleError, temple, type Space } from '@schaefchens/temple-web';
import { selfTest, type Check } from '@schaefchens/temple/self-test';
import { english, toIndices, toWords } from '@schaefchens/temple/words';
import { useEffect, useState, type FormEvent } from 'react';

const VAULT = 'demo.vault';
const SCHEMA = `CREATE VIRTUAL TABLE IF NOT EXISTS note USING fts5(body);
  CREATE TABLE IF NOT EXISTS file (id INTEGER PRIMARY KEY, object INTEGER NOT NULL, name TEXT NOT NULL, size INTEGER NOT NULL);`;

export function App() {
  const selfTesting = new URLSearchParams(location.search).has('self-test');
  return <TempleProvider temple={temple}>{selfTesting ? <SelfTest /> : <Demo />}</TempleProvider>;
}

function Demo() {
  const vault = useVault(VAULT);
  const { lock } = vault;
  useEffect(() => {
    const hidden = () => {
      if (document.visibilityState === 'hidden') void lock();
    };
    document.addEventListener('visibilitychange', hidden);
    return () => document.removeEventListener('visibilitychange', hidden);
  }, [lock]);

  return (
    <main>
      {vault.status === 'opening' && <p>Opening the vault…</p>}
      {vault.status === 'failed' && (
        <p data-testid="notice" className="error">
          {TempleError.is(vault.error, 'InUse')
            ? 'The vault is open in another tab. Close it there to use it here.'
            : message(vault.error)}
        </p>
      )}
      {vault.status === 'missing' && (
        <NewWords
          title="A new vault"
          action="Create vault"
          busy="Creating the vault – writing its noise…"
          onConfirm={async (words) => {
            await vault.create(words);
            // Asks the browser not to evict the vault when storage runs low – without waiting:
            // Firefox asks the user.
            void navigator.storage.persist?.();
          }}
        />
      )}
      {vault.status === 'locked' && <Unlock vault={vault} />}
      {vault.status === 'open' && <Notes space={vault.space} lock={vault.lock} />}
    </main>
  );
}

/** Fresh words to write down, and what they're for. */
function NewWords(props: {
  title: string;
  action: string;
  busy: string;
  onConfirm(words: number[]): Promise<void>;
  onCancel?(): void;
}) {
  const [words, setWords] = useState<number[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => void temple.generateWords().then(setWords), []);
  if (busy) return <p>{props.busy}</p>;
  return (
    <>
      <h1>{props.title}</h1>
      <p>Write these 12 words down. They are the only way in: nobody can recover them.</p>
      <p data-testid="words" className="words">
        {words ? toWords(words, english).join(' ') : '…'}
      </p>
      <div className="row">
        <button
          disabled={!words}
          onClick={async () => {
            setBusy(true);
            try {
              await props.onConfirm(words!);
            } catch (e) {
              setError(message(e));
              setBusy(false);
            }
          }}
        >
          {props.action}
        </button>
        {props.onCancel && <button onClick={props.onCancel}>Cancel</button>}
      </div>
      {error && <p className="error">{error}</p>}
    </>
  );
}

function Unlock({ vault }: { vault: VaultHandle }) {
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [newSpace, setNewSpace] = useState(false);

  if (newSpace) {
    return (
      <NewWords
        title="A new space"
        action="Create space"
        busy="Creating the space…"
        onConfirm={(words) => vault.createSpace(words)}
        onCancel={() => setNewSpace(false)}
      />
    );
  }
  if (busy) return <p>Unlocking…</p>;

  const unlock = async (event: FormEvent) => {
    event.preventDefault();
    const words = toIndices(text, english);
    if (!words || !(await temple.wordsAreValid(words))) {
      setNotice('These aren’t 12 valid words – check for a typo.');
      return;
    }
    setBusy(true);
    try {
      if (!(await vault.unlock(words))) {
        setNotice('These words open no space.');
        setBusy(false);
      }
    } catch (e) {
      setNotice(message(e));
      setBusy(false);
    }
  };

  return (
    <form onSubmit={unlock} style={{ display: 'contents' }}>
      <h1>Locked</h1>
      <textarea
        data-testid="words-input"
        value={text}
        onChange={(event) => setText(event.target.value)}
        onKeyDown={(event) => {
          if (event.key === 'Enter' && !event.shiftKey) void unlock(event);
        }}
        placeholder="12 words"
        autoCapitalize="none"
        autoCorrect="off"
        spellCheck={false}
        rows={3}
      />
      <div className="row">
        <button type="submit">Unlock</button>
        <button type="button" onClick={() => setNewSpace(true)}>
          New space…
        </button>
      </div>
      {notice && (
        <p data-testid="notice" className="notice">
          {notice}
        </p>
      )}
    </form>
  );
}

function Notes({ space, lock }: { space: Space; lock(): Promise<void> }) {
  const [ready, setReady] = useState(false);
  const [mode, setMode] = useState<'notes' | 'change-words'>('notes');
  const [draft, setDraft] = useState('');
  const [search, setSearch] = useState('');
  const [notice, setNotice] = useState<string | null>(null);
  useEffect(() => {
    void space.executeBatch(SCHEMA).then(() => setReady(true), (e) => setNotice(message(e)));
  }, [space]);
  const execute = useExecute(space);
  const terms = ftsQuery(search);
  const notes = useQuery<{ id: number; body: string }>(
    ready ? space : null,
    terms
      ? 'SELECT rowid AS id, body FROM note WHERE note MATCH ? ORDER BY rowid DESC'
      : 'SELECT rowid AS id, body FROM note ORDER BY rowid DESC',
    terms ? [terms] : [],
  );
  const files = useQuery<{ id: number; object: number; name: string; size: number }>(
    ready ? space : null,
    'SELECT id, object, name, size FROM file ORDER BY id DESC',
  );
  const run = (task: () => Promise<unknown>) => void task().catch((e) => setNotice(message(e)));

  if (mode === 'change-words') {
    return (
      <NewWords
        title="New words for this space"
        action="Use these words"
        busy="Changing the words…"
        onConfirm={async (words) => {
          await space.changeWords(words);
          setNotice('Words changed: the old ones open nothing now.');
          setMode('notes');
        }}
        onCancel={() => setMode('notes')}
      />
    );
  }

  return (
    <>
      <div className="row">
        <h1 className="grow">Notes</h1>
        <button onClick={() => run(lock)}>Lock</button>
      </div>
      <form
        className="row"
        onSubmit={(event) => {
          event.preventDefault();
          run(async () => {
            if (!draft.trim()) return;
            await execute('INSERT INTO note (body) VALUES (?)', [draft.trim()]);
            setDraft('');
          });
        }}
      >
        <input
          data-testid="note-input"
          className="grow"
          value={draft}
          onChange={(event) => setDraft(event.target.value)}
          placeholder="A note"
        />
        <button type="submit">Add note</button>
      </form>
      <input
        data-testid="search-input"
        type="search"
        value={search}
        onChange={(event) => setSearch(event.target.value)}
        placeholder="Search"
      />
      {notes.rows.map((note) => (
        <div key={note.id} className="row">
          <span className="grow">{note.body}</span>
          <button onClick={() => run(() => execute('DELETE FROM note WHERE rowid = ?', [note.id]))}>Delete</button>
        </div>
      ))}
      <h2>Files</h2>
      <label className="row">
        <span>Attach a file:</span>
        <input
          data-testid="file-input"
          type="file"
          onChange={(event) => {
            const file = event.target.files?.[0];
            event.target.value = '';
            if (!file) return;
            run(async () => {
              const object = await space.putObject(new Uint8Array(await file.arrayBuffer()));
              await execute('INSERT INTO file (object, name, size) VALUES (?, ?, ?)', [object, file.name, file.size]);
            });
          }}
        />
      </label>
      {files.rows.map((file) => (
        <div key={file.id} className="row">
          <span className="grow">
            {file.name} · {file.size < 1024 ? `${file.size} bytes` : `${Math.round(file.size / 1024)} KiB`}
          </span>
          <button
            onClick={() =>
              run(async () => {
                const bytes = await space.readObject(file.object);
                const url = URL.createObjectURL(new Blob([bytes as Uint8Array<ArrayBuffer>]));
                const link = Object.assign(document.createElement('a'), { href: url, download: file.name });
                link.click();
                URL.revokeObjectURL(url);
              })
            }
          >
            Save
          </button>
          <button
            onClick={() =>
              run(async () => {
                await space.deleteObject(file.object);
                await execute('DELETE FROM file WHERE id = ?', [file.id]);
              })
            }
          >
            Remove
          </button>
        </div>
      ))}
      <h2>This space</h2>
      <button onClick={() => setMode('change-words')}>Change words…</button>
      {notice && (
        <p data-testid="notice" className="notice">
          {notice}
        </p>
      )}
    </>
  );
}

/** Temple's self-test in this browser, for the E2E run: `?self-test`. */
function SelfTest() {
  const [checks, setChecks] = useState<Check[]>([]);
  const [done, setDone] = useState(false);
  useEffect(() => {
    const files = { path: (name: string) => name, remove };
    void selfTest(temple, files, (check) => setChecks((all) => [...all, check])).finally(() => setDone(true));
  }, []);
  const failed = checks.filter((check) => !check.ok).length;
  return (
    <main>
      <h1>Self-test</h1>
      {checks.map((check) => (
        <div key={check.name} className={`check ${check.ok ? 'passed' : 'error'}`}>
          {check.ok ? '✓' : '✗'} {check.name} · {check.seconds.toFixed(1)} s{check.message ? `\n${check.message}` : ''}
        </div>
      ))}
      <h2 data-testid="self-test-result">
        {!done ? 'Running…' : failed ? `${failed} of ${checks.length} checks failed` : `All ${checks.length} checks passed`}
      </h2>
    </main>
  );
}

/** Removes a file of the origin private file system – once Temple's worker has let go of it. */
async function remove(name: string) {
  const directory = await navigator.storage.getDirectory();
  for (let attempt = 0; ; attempt++) {
    try {
      await directory.removeEntry(name);
      return;
    } catch (error) {
      if (error instanceof DOMException && error.name === 'NotFoundError') return;
      if (attempt === 50) throw error;
      await new Promise((resolve) => setTimeout(resolve, 100));
    }
  }
}

/** A search as FTS5 terms: each word as a quoted prefix. */
function ftsQuery(search: string): string {
  return search
    .split(/\s+/)
    .filter(Boolean)
    .map((term) => `"${term.replace(/"/g, '""')}"*`)
    .join(' ');
}

function message(error: unknown): string {
  return TempleError.is(error) ? error.message : error instanceof Error ? error.message : String(error);
}
