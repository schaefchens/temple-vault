import type { Page } from '@playwright/test';
import { expect, test } from './fixtures';
import { readFile } from 'node:fs/promises';

// The demo as a person uses it: one vault, two spaces, notes, a file, locking – and what
// the web adds: reloads, a second tab, and working offline.

async function wordsOnScreen(page: Page) {
  await expect(page.getByTestId('words')).not.toHaveText('…');
  return (await page.getByTestId('words').textContent())!.trim();
}

async function unlock(page: Page, words: string) {
  await page.getByTestId('words-input').fill(words);
  await page.getByRole('button', { name: 'Unlock' }).click();
}

test('notes in two spaces, through locks, reloads and a second tab', async ({ page, context }) => {
  await page.goto('/');
  const first = await wordsOnScreen(page);
  await page.getByRole('button', { name: 'Create vault' }).click();
  // The vault writes its reserve of noise first: hundreds of megabytes.
  await expect(page.getByRole('heading', { name: 'Notes' })).toBeVisible({ timeout: 180_000 });

  // Notes, full-text search, a file in and out.
  await page.getByTestId('note-input').fill('Buy milk');
  await page.getByRole('button', { name: 'Add note' }).click();
  await expect(page.getByText('Buy milk')).toBeVisible();
  await page.getByTestId('search-input').fill('zebra');
  await expect(page.getByText('Buy milk')).toBeHidden();
  await page.getByTestId('search-input').fill('mil');
  await expect(page.getByText('Buy milk')).toBeVisible();
  await page.getByTestId('search-input').fill('');
  await page.getByTestId('file-input').setInputFiles({
    name: 'hello.txt',
    mimeType: 'text/plain',
    buffer: Buffer.from('hello from a file'),
  });
  await expect(page.getByText('hello.txt · 17 bytes')).toBeVisible();
  const download = page.waitForEvent('download');
  await page.getByRole('button', { name: 'Save' }).click();
  expect(await readFile((await (await download).path())!, 'utf8')).toBe('hello from a file');

  // Locking; words that open nothing, a typo, the right words.
  await page.getByRole('button', { name: 'Lock' }).click();
  await expect(page.getByRole('heading', { name: 'Locked' })).toBeVisible();
  await unlock(page, 'abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about');
  await expect(page.getByTestId('notice')).toHaveText('These words open no space.');
  await unlock(page, 'abandon abandon');
  await expect(page.getByTestId('notice')).toHaveText(/These aren.t 12 valid words/);
  await unlock(page, first);
  await expect(page.getByText('Buy milk')).toBeVisible();

  // A second space under other words sees nothing of the first, and the other way round.
  await page.getByRole('button', { name: 'Lock' }).click();
  await page.getByRole('button', { name: 'New space…' }).click();
  const second = await wordsOnScreen(page);
  await page.getByRole('button', { name: 'Create space' }).click();
  await expect(page.getByRole('heading', { name: 'Notes' })).toBeVisible();
  await expect(page.getByText('Buy milk')).toBeHidden();
  await page.getByTestId('note-input').fill('Hidden note');
  await page.getByRole('button', { name: 'Add note' }).click();
  await expect(page.getByText('Hidden note')).toBeVisible();
  await page.getByRole('button', { name: 'Lock' }).click();
  await unlock(page, first);
  await expect(page.getByText('Buy milk')).toBeVisible();
  await expect(page.getByText('Hidden note')).toBeHidden();

  // Hiding the page locks the space.
  await page.evaluate(() => {
    Object.defineProperty(document, 'visibilityState', { configurable: true, get: () => 'hidden' });
    document.dispatchEvent(new Event('visibilitychange'));
  });
  await expect(page.getByRole('heading', { name: 'Locked' })).toBeVisible();

  // A vault opens in one tab at a time.
  const other = await context.newPage();
  await other.goto('/');
  await expect(other.getByTestId('notice')).toHaveText(/open in another tab/, { timeout: 15_000 });
  await other.close();

  // After a reload both spaces open again with their words.
  await page.reload();
  await unlock(page, second);
  await expect(page.getByText('Hidden note')).toBeVisible();
  await expect(page.getByText('Buy milk')).toBeHidden();

  // Offline, the service worker serves the page, the worker and the wasm module.
  await page.evaluate(() => navigator.serviceWorker.ready.then(() => undefined));
  await context.setOffline(true);
  await page.reload();
  await unlock(page, first);
  await expect(page.getByText('Buy milk')).toBeVisible();
  await context.setOffline(false);
});
