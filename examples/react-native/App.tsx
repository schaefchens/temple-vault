// Notes on Temple: one vault in the app's documents, spaces opened by 12 words, notes in each
// space's database with full-text search, files as objects. Leaving the app locks the space,
// and a cover hides it while the app isn't active. The link temple-demo://self-test offers
// Temple's self-test on this device instead.

import { TempleProvider, changed, useExecute, useQuery, useVault, type VaultHandle } from '@schaefchens/temple-react';
import { TempleError, temple, type NewWords, type Space } from '@schaefchens/temple-react-native';
import { selfTest, type Check } from '@schaefchens/temple/self-test';
import { english, toIndices, toWords } from '@schaefchens/temple/words';
import { File, Paths } from 'expo-file-system';
import * as SecureStore from 'expo-secure-store';
import { StatusBar } from 'expo-status-bar';
import { useCallback, useEffect, useRef, useState } from 'react';
import { ActivityIndicator, AppState, Linking, Pressable, ScrollView, StyleSheet, Text, TextInput, View } from 'react-native';
import { SafeAreaProvider, SafeAreaView } from 'react-native-safe-area-context';

const VAULT = documentPath('demo.vault');
/** The phrase key for quick unlock, behind the device's biometrics or passcode. */
const KEY = 'temple-demo.phrase-key';
/** Whether a key is kept – asking for the key itself would show the prompt. */
const HAS_KEY = 'temple-demo.has-key';
const SCHEMA = `CREATE VIRTUAL TABLE IF NOT EXISTS note USING fts5(body);
  CREATE TABLE IF NOT EXISTS file (id INTEGER PRIMARY KEY, object INTEGER NOT NULL, name TEXT NOT NULL, size INTEGER NOT NULL);`;

export default function App() {
  const [selfTesting, setSelfTesting] = useState(false);
  useEffect(() => {
    const route = (url?: string | null) => setSelfTesting(url?.includes('self-test') ?? false);
    void Linking.getInitialURL().then(route);
    const subscription = Linking.addEventListener('url', ({ url }) => route(url));
    return () => subscription.remove();
  }, []);
  return (
    <SafeAreaProvider>
      <SafeAreaView style={styles.screen}>
        <StatusBar style="auto" />
        <TempleProvider temple={temple}>{selfTesting ? <SelfTest /> : <Demo />}</TempleProvider>
      </SafeAreaView>
    </SafeAreaProvider>
  );
}

function Demo() {
  const vault = useVault(VAULT);
  const { lock } = vault;
  // Not active – the app switcher, a system sheet – the screen's snapshot shows only a cover.
  const [active, setActive] = useState(AppState.currentState === 'active');
  useEffect(() => {
    const subscription = AppState.addEventListener('change', (state) => {
      setActive(state === 'active');
      if (state === 'background') void lock();
    });
    // A change before the listener came is in the current state.
    setActive(AppState.currentState === 'active');
    return () => subscription.remove();
  }, [lock]);
  // Every backup would be another copy of the vault: left out whenever it opens – a vault made
  // before the demo did so, too.
  const opened = vault.status === 'locked' || vault.status === 'open' ? vault.vault : null;
  useEffect(() => {
    if (opened) temple.excludeFromBackup(VAULT).catch((e) => console.warn(message(e)));
  }, [opened]);

  if (!active) return <View style={styles.cover} />;
  switch (vault.status) {
    case 'opening':
      return <Busy label="Opening the vault…" />;
    case 'failed':
      return <Text style={styles.error}>{message(vault.error)}</Text>;
    case 'missing':
      return (
        <FreshWords
          title="A new vault"
          action="Create vault"
          busy="Creating the vault – writing its noise…"
          onConfirm={(words) => vault.create(words)}
        />
      );
    case 'locked':
      return <Unlock vault={vault} />;
    case 'open':
      return <Notes space={vault.space} lock={vault.lock} />;
  }
}

/** Fresh words to write down, and what they're for. */
function FreshWords(props: {
  title: string;
  action: string;
  busy: string;
  onConfirm(words: NewWords): Promise<void>;
  onCancel?(): void;
}) {
  const [words, setWords] = useState<NewWords | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => void temple.generateWords().then(setWords), []);
  if (busy) return <Busy label={props.busy} />;
  return (
    <View style={styles.card}>
      <Text style={styles.title}>{props.title}</Text>
      <Text>Write these 12 words down. They are the only way in: nobody can recover them.</Text>
      <Text testID="words" selectable style={styles.words}>
        {words ? toWords(words.indices, english).join(' ') : '…'}
      </Text>
      <Button
        label={props.action}
        onPress={async () => {
          if (!words) return;
          setBusy(true);
          try {
            await props.onConfirm(words);
          } catch (e) {
            setError(message(e));
            setBusy(false);
          }
        }}
      />
      {props.onCancel && <Button label="Cancel" onPress={props.onCancel} />}
      {error && <Text style={styles.error}>{error}</Text>}
    </View>
  );
}

function Unlock({ vault }: { vault: VaultHandle }) {
  const [text, setText] = useState('');
  const [busy, setBusy] = useState(false);
  const [notice, setNotice] = useState<string | null>(null);
  const [newSpace, setNewSpace] = useState(false);
  const [hasKey, setHasKey] = useState(false);
  useEffect(() => void SecureStore.getItemAsync(HAS_KEY).then((kept) => setHasKey(kept === '1')), []);

  if (newSpace) {
    return (
      <FreshWords
        title="A new space"
        action="Create space"
        busy="Creating the space…"
        onConfirm={(words) => vault.createSpace(words)}
        onCancel={() => setNewSpace(false)}
      />
    );
  }
  if (busy) return <Busy label="Unlocking…" />;

  const attempt = async (open: () => Promise<boolean>, failed: string) => {
    setBusy(true);
    try {
      if (!(await open())) {
        setNotice(failed);
        setBusy(false);
      }
    } catch (e) {
      setNotice(message(e));
      setBusy(false);
    }
  };
  const unlock = async () => {
    const words = toIndices(text, english);
    if (!words || !(await temple.wordsAreValid(words))) {
      setNotice('These aren’t 12 valid words – check for a typo.');
      return;
    }
    await attempt(() => vault.unlock(words), 'These words open no space.');
  };
  const quickUnlock = async () => {
    const key = await SecureStore.getItemAsync(KEY, { requireAuthentication: true, authenticationPrompt: 'Unlock' });
    if (key) await attempt(() => vault.unlockWithKey(fromHex(key)), 'The kept key opens nothing any more.');
  };

  return (
    <View style={styles.card}>
      <Text style={styles.title}>Locked</Text>
      <TextInput
        testID="words-input"
        value={text}
        onChangeText={setText}
        placeholder="12 words"
        autoCapitalize="none"
        autoCorrect={false}
        multiline
        submitBehavior="blurAndSubmit"
        returnKeyType="go"
        onSubmitEditing={() => void unlock()}
        style={styles.input}
      />
      <View style={styles.row}>
        <Button label="Unlock" onPress={unlock} />
        {text !== '' && <Button label="Clear" onPress={() => setText('')} />}
      </View>
      {hasKey && <Button label="Quick unlock" onPress={() => void quickUnlock().catch((e) => setNotice(message(e)))} />}
      <Button label="New space…" onPress={() => setNewSpace(true)} />
      {notice && (
        <Text testID="notice" style={styles.notice}>
          {notice}
        </Text>
      )}
    </View>
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
  const write = useWrites();
  const run = (task: () => Promise<unknown>) => void task().catch((e) => setNotice(message(e)));
  const add = () =>
    run(async () => {
      if (!draft.trim()) return;
      await write(() => execute('INSERT INTO note (body) VALUES (?)', [draft.trim()]));
      setDraft('');
    });

  if (mode === 'change-words') {
    return (
      <FreshWords
        title="New words for this space"
        action="Use these words"
        busy="Changing the words…"
        onConfirm={async (words) => {
          await space.changeWords(words);
          // A kept phrase key belonged to the old words.
          await SecureStore.deleteItemAsync(KEY);
          await SecureStore.deleteItemAsync(HAS_KEY);
          setNotice('Words changed: the old ones open nothing now.');
          setMode('notes');
        }}
        onCancel={() => setMode('notes')}
      />
    );
  }

  return (
    <ScrollView contentContainerStyle={styles.card}>
      <View style={styles.row}>
        <Text style={[styles.title, styles.grow]}>Notes</Text>
        <Button label="Lock" onPress={() => run(lock)} />
      </View>
      <TextInput
        testID="note-input"
        value={draft}
        onChangeText={setDraft}
        placeholder="A note"
        returnKeyType="done"
        onSubmitEditing={add}
        style={styles.input}
      />
      <Button label="Add note" onPress={add} />
      <TextInput
        testID="search-input"
        value={search}
        onChangeText={setSearch}
        placeholder="Search"
        returnKeyType="search"
        style={styles.input}
      />
      {notes.rows.map((note) => (
        <View key={note.id} style={styles.row}>
          <Text style={styles.note}>{note.body}</Text>
          <Button
            label="Delete"
            onPress={() => run(() => write(() => execute('DELETE FROM note WHERE rowid = ?', [note.id])))}
          />
        </View>
      ))}
      <Text style={styles.subtitle}>Files</Text>
      <Button
        label="Attach sample file"
        onPress={() =>
          run(async () => {
            // In pieces, as a download arrives: a large file never sits in memory whole – and outside any
            // transaction, so that notes needn't wait for it.
            const bytes = sample(256 * 1024);
            const writer = await space.writeObject();
            for (let at = 0; at < bytes.length; at += 64 * 1024) await writer.write(bytes.subarray(at, at + 64 * 1024));
            await write(() =>
              transaction(space, async () => {
                const object = await writer.finish();
                await space.execute('INSERT INTO file (object, name, size) VALUES (?, ?, ?)', [object, `sample-${object}.bin`, bytes.length]);
              }),
            );
          })
        }
      />
      {files.rows.map((file) => (
        <View key={file.id} style={styles.row}>
          <Text style={styles.note}>
            {file.name} · {Math.round(file.size / 1024)} KiB
          </Text>
          <Button
            label="Check"
            onPress={() =>
              run(async () => {
                const bytes = await space.readObject(file.object);
                const intact = equal(bytes, sample(file.size));
                setNotice(`${file.name}: ${intact ? 'intact' : 'damaged'}`);
              })
            }
          />
          <Button
            label="Remove"
            onPress={() =>
              run(() =>
                write(() =>
                  transaction(space, async () => {
                    await space.deleteObject(file.object);
                    await space.execute('DELETE FROM file WHERE id = ?', [file.id]);
                  }),
                ),
              )
            }
          />
        </View>
      ))}
      <Text style={styles.subtitle}>This space</Text>
      <Button
        label="Keep quick unlock"
        onPress={() =>
          run(async () => {
            const key = toHex(await space.phraseKey());
            await SecureStore.setItemAsync(KEY, key, { requireAuthentication: true });
            await SecureStore.setItemAsync(HAS_KEY, '1');
            setNotice(
              'Quick unlock is on: the key sits behind your biometrics or passcode, and opens this space for whoever passes them – keep it for your everyday space.',
            );
          })
        }
      />
      <Button label="Change words…" onPress={() => setMode('change-words')} />
      {notice && (
        <Text testID="notice" style={styles.notice}>
          {notice}
        </Text>
      )}
    </ScrollView>
  );
}

/** Temple's self-test on this device, for the E2E run: temple-demo://self-test – once asked for here: a link alone could start it, and it writes two vaults of some hundred megabytes. */
function SelfTest() {
  const [asked, setAsked] = useState(false);
  if (!asked) {
    return (
      <View style={styles.card}>
        <Text style={styles.title}>Self-test</Text>
        <Text>Runs Temple's checks on this device: two vaults of some hundred megabytes, deleted at the end.</Text>
        <Button label="Run self-test" onPress={() => setAsked(true)} />
      </View>
    );
  }
  return <SelfTestRun />;
}

function SelfTestRun() {
  const [checks, setChecks] = useState<Check[]>([]);
  const [done, setDone] = useState(false);
  useEffect(() => {
    const files = {
      path: documentPath,
      async remove(path: string) {
        const file = new File(Paths.document, path.slice(path.lastIndexOf('/') + 1));
        if (file.exists) file.delete();
      },
    };
    void selfTest(temple, files, (check) => setChecks((all) => [...all, check])).finally(() => setDone(true));
  }, []);
  const failed = checks.filter((check) => !check.ok).length;
  return (
    <ScrollView contentContainerStyle={styles.card}>
      <Text style={styles.title}>Self-test</Text>
      {/* Above the checks, so that it stays on screen however many there are. */}
      <Text testID="self-test-result" style={styles.subtitle}>
        {!done ? 'Running…' : failed ? `${failed} of ${checks.length} checks failed` : `All ${checks.length} checks passed`}
      </Text>
      {checks.map((check) => (
        <Text key={check.name} style={check.ok ? styles.passed : styles.error}>
          {check.ok ? '✓' : '✗'} {check.name} · {check.seconds.toFixed(1)} s{check.message ? `\n${check.message}` : ''}
        </Text>
      ))}
    </ScrollView>
  );
}

function Busy({ label }: { label: string }) {
  return (
    <View style={styles.card}>
      <ActivityIndicator />
      <Text>{label}</Text>
    </View>
  );
}

function Button({ label, onPress }: { label: string; onPress(): void }) {
  return (
    <Pressable accessibilityRole="button" onPress={onPress} style={({ pressed }) => [styles.button, pressed && styles.pressed]}>
      <Text style={styles.buttonLabel}>{label}</Text>
    </Pressable>
  );
}

/**
 * Runs writes one after another: a transaction is the whole space's, and a write made while one is open joins it – and
 * is rolled back with it.
 */
function useWrites() {
  const last = useRef<Promise<unknown>>(Promise.resolve());
  return useCallback(<T,>(write: () => Promise<T>): Promise<T> => {
    const done = last.current.then(write);
    last.current = done.catch(() => {});
    return done;
  }, []);
}

/**
 * Runs statements in one transaction, rolled back if one fails: an object and its row go together. Queries run again once
 * it has ended – committed or rolled back – so they never show what it wrote before then.
 */
async function transaction(space: Space, work: () => Promise<void>) {
  await space.execute('BEGIN');
  try {
    await work();
    await space.execute('COMMIT');
  } catch (error) {
    await space.execute('ROLLBACK').catch(() => {});
    throw error;
  } finally {
    changed(space);
  }
}

/** A file path in the app's documents, as Temple takes it. */
function documentPath(name: string): string {
  return decodeURIComponent(new File(Paths.document, name).uri.replace(/^file:\/\//, ''));
}

/** A search as FTS5 terms: each word as a quoted prefix. */
function ftsQuery(search: string): string {
  return search
    .split(/\s+/)
    .filter(Boolean)
    .map((term) => `"${term.replace(/"/g, '""')}"*`)
    .join(' ');
}

/** Bytes a sample file of `size` holds – the same every time, so they can be checked. */
function sample(size: number): Uint8Array {
  const bytes = new Uint8Array(size);
  for (let i = 0; i < size; i++) bytes[i] = (i * 31 + (i >> 8)) & 0xff;
  return bytes;
}

function equal(a: Uint8Array, b: Uint8Array): boolean {
  return a.length === b.length && a.every((byte, i) => byte === b[i]);
}

function toHex(bytes: Uint8Array): string {
  return Array.from(bytes, (byte) => byte.toString(16).padStart(2, '0')).join('');
}

function fromHex(hex: string): Uint8Array {
  return new Uint8Array(hex.match(/../g)?.map((pair) => parseInt(pair, 16)) ?? []);
}

function message(error: unknown): string {
  return TempleError.is(error) ? error.message : error instanceof Error ? error.message : String(error);
}

const styles = StyleSheet.create({
  screen: { flex: 1 },
  cover: { flex: 1, backgroundColor: '#2f6fde' },
  card: { padding: 16, gap: 12 },
  row: { flexDirection: 'row', alignItems: 'center', gap: 8 },
  title: { fontSize: 22, fontWeight: '600' },
  grow: { flex: 1 },
  subtitle: { fontSize: 17, fontWeight: '600', marginTop: 8 },
  words: { fontSize: 18, lineHeight: 28, padding: 12, borderRadius: 8, backgroundColor: '#8881' },
  input: { borderWidth: 1, borderColor: '#8886', borderRadius: 8, padding: 10, fontSize: 16, minHeight: 44 },
  note: { flex: 1, fontSize: 16 },
  button: { backgroundColor: '#2f6fde', borderRadius: 8, paddingVertical: 10, paddingHorizontal: 14, alignSelf: 'flex-start' },
  pressed: { opacity: 0.7 },
  buttonLabel: { color: 'white', fontSize: 16, fontWeight: '500' },
  notice: { color: '#b26b00' },
  error: { color: '#c62828' },
  passed: { color: '#2e7d32' },
});
