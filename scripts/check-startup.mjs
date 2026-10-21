// Run with npm run dev; use the same optional Playwright module override as the Android check.
import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
const { chromium } = await import(process.env.MDASH_PLAYWRIGHT_MODULE ?? 'playwright');
const origin = process.env.MDASH_PREVIEW_URL ?? 'http://127.0.0.1:1420';
const fixture = await readFile(new URL('../preview/fixture.js', import.meta.url), 'utf8');
const browser = await chromium.launch({ channel: 'chrome', headless: true });
try {
  for (const scenario of ['read-failure', 'version-failure', 'refresh-failure', 'healthy', 'browser']) {
    const page = await browser.newPage();
    const errors = [];
    page.on('pageerror', error => errors.push(error.message));
    if (scenario !== 'browser') {
      await page.route('**/preview/fixture.js', route => route.fulfill({
        contentType: 'text/javascript',
        body: `${fixture}
          window.isTauri = true;
          let refreshFailed = false;
          const original = window.__TAURI_INTERNALS__.invoke;
          window.__TAURI_INTERNALS__.invoke = async (command, args) => {
            if (${JSON.stringify(scenario)} === 'read-failure' && command === 'get_dashboard') throw new Error('Collector is still starting');
            if (${JSON.stringify(scenario)} === 'version-failure' && command === 'plugin:app|version') throw new Error('Version unavailable');
            if (${JSON.stringify(scenario)} === 'refresh-failure' && command === 'refresh_dashboard' && !refreshFailed) { refreshFailed = true; throw new Error('Collector unavailable'); }
            return original(command, args);
          };`,
      }));
    }
    await page.goto(new URL(scenario === 'browser' ? '/' : '/preview/', origin).href);
    if (scenario === 'browser') {
      await page.getByRole('alert').waitFor();
      assert.match(await page.getByRole('alert').textContent(), /Open the installed desktop application/);
    } else if (scenario === 'read-failure') {
      await page.getByRole('alert').waitFor();
      assert.match(await page.getByRole('alert').textContent(), /local collector/i);
      await page.evaluate(() => window.previewUpdate());
      await page.locator('.primary-card').waitFor();
      assert.equal(await page.getByRole('alert').count(), 0, 'Healthy native updates must clear the startup error');
    } else {
      await page.locator('.primary-card').waitFor({ timeout: 3000 });
      assert.equal(await page.getByRole('alert').count(), 0, 'Installed dashboards must not show the browser warning');
      if (scenario === 'refresh-failure') {
        await page.getByRole('button', { name: 'Refresh accounts' }).click();
        await page.getByRole('alert').waitFor();
        assert.match(await page.getByRole('alert').textContent(), /local collector/i);
        await page.getByRole('button', { name: 'Refresh accounts' }).click();
        await page.getByRole('alert').waitFor({ state: 'detached' });
      }
    }
    assert.deepEqual(errors, [], 'Startup must not leave unhandled errors');
    console.log(`PASS ${scenario}`);
    await page.close();
  }
} finally {
  await browser.close();
}
