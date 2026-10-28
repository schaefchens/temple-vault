import { expect, test } from './fixtures';

// Temple's self-test in this engine: the API end to end, two real vaults in the origin
// private file system.
test('the self-test passes', async ({ page }) => {
  await page.goto('/?self-test');
  await page.getByRole('button', { name: 'Run self-test' }).click();
  const result = page.getByTestId('self-test-result');
  await expect(result).toHaveText(/checks (passed|failed)/, { timeout: 280_000 });
  const failed = await page.locator('.check.error').allTextContents();
  expect(failed, failed.join('\n')).toEqual([]);
  await expect(result).toHaveText(/All \d+ checks passed/);
});
