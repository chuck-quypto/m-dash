// Synthetic browser checks; no provider accounts, credentials, or relay connections.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
const { chromium } = await import(process.env.MDASH_PLAYWRIGHT_MODULE ?? 'playwright');
const origin = process.env.MDASH_PREVIEW_URL ?? 'http://127.0.0.1:1420';
const fixture = await readFile(new URL('../preview/fixture.js', import.meta.url), 'utf8');
const browser = await chromium.launch({ channel: 'chrome', headless: true });
try {
  const page = await browser.newPage({ timezoneId: 'America/New_York' });
  await page.route('**/preview/fixture.js', route => route.fulfill({ contentType: 'text/javascript', body: fixture.replace('claudeError:null,claudeUsageError:null', 'claudeError:"Synthetic receiver error",claudeUsageError:"Synthetic account error"') }));
  await page.goto(new URL('/preview/', origin).href);
  await page.locator('.primary-card').waitFor();
  await page.locator('nav [data-page="connections"]').click();
  await page.getByText('Account usage: Synthetic account error', { exact: true }).waitFor();
  await page.getByText('Local feed: Synthetic receiver error', { exact: true }).waitFor();
  assert.equal(await page.locator('#code-label').isVisible(), false);
  assert.equal(await page.locator('#code-label').evaluate(el => getComputedStyle(el).display), 'none');
  await page.locator('#compact-mode').focus();
  await page.locator('#compact-mode').evaluate(el => { window.savedSelect = el; });
  await page.evaluate(() => window.previewUpdate());
  assert.equal(await page.evaluate(() => window.savedSelect === document.activeElement && window.savedSelect.isConnected), true);
  await page.locator('#compact-mode').blur();
  const overview = page.locator('nav [data-page="overview"]');
  await overview.hover(); await page.mouse.down();
  await overview.evaluate(el => { window.savedButton = el; });
  await page.evaluate(() => window.previewUpdate());
  assert.equal(await page.evaluate(() => window.savedButton.isConnected), true);
  await page.mouse.up();
  await page.locator('.primary-card').waitFor();
  console.log('PASS dropdown focus, pointer click, separate Claude errors, hidden pairing code');
  await page.clock.install({ time: new Date('2026-10-31T12:00:00Z') });
  await page.goto(new URL('/preview/?compact', origin).href);
  await page.locator('.allowance').first().waitFor();
  assert.match(await page.locator('.timezone').textContent(), /EDT/);
  await page.clock.setSystemTime(new Date('2026-11-02T12:00:00Z'));
  await page.evaluate(() => window.previewUpdate());
  assert.match(await page.locator('.timezone').textContent(), /EST/);
  console.log('PASS compact strip recomputes its zone after DST');
} finally { await browser.close(); }
