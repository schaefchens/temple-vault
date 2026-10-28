import { test as base, chromium, firefox, webkit, type BrowserContext, type Page } from '@playwright/test';
import { mkdtemp, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

// Each test in a fresh, persistent profile: WebKit's ephemeral contexts have no origin
// private file system. WebKit keeps that file system outside the profile, so each test
// also starts by emptying it – and ends that way, as vaults take hundreds of megabytes.
export const test = base.extend<{ context: BrowserContext }>({
  context: async ({ browserName, baseURL }, use) => {
    const profile = await mkdtemp(join(tmpdir(), 'temple-e2e-'));
    const context = await { chromium, firefox, webkit }[browserName].launchPersistentContext(profile, { baseURL });
    await use(context);
    await context.close();
    await rm(profile, { recursive: true, force: true });
  },
  page: async ({ context }, use) => {
    const page = context.pages()[0] ?? (await context.newPage());
    await emptyStorage(page);
    await use(page);
    await emptyStorage(page);
  },
});

/**
 * Removes every file of the origin's private file system, from a page that opens none –
 * once the worker of the page before has let go of them.
 */
async function emptyStorage(page: Page) {
  await page.goto('/icon.svg');
  await page.evaluate(async () => {
    const root = await navigator.storage.getDirectory();
    const names: string[] = [];
    for await (const name of (root as unknown as { keys(): AsyncIterable<string> }).keys()) names.push(name);
    for (const name of names) {
      for (let attempt = 0; ; attempt++) {
        try {
          await root.removeEntry(name, { recursive: true });
          break;
        } catch (error) {
          if (attempt === 50) throw error;
          await new Promise((resolve) => setTimeout(resolve, 100));
        }
      }
    }
  });
}

export { expect } from '@playwright/test';
