// Requires Playwright installed separately, like check-startup.mjs.
import assert from 'node:assert/strict';
const { chromium } = await import(process.env.MDASH_PLAYWRIGHT_MODULE ?? 'playwright');
const browser = await chromium.launch({ channel: 'chrome', headless: true });
try {
  const page = await browser.newPage({ viewport: { width: 1100, height: 500 }, deviceScaleFactor: 2 });
  await page.goto(`${process.env.MDASH_PREVIEW_URL ?? 'http://127.0.0.1:1420'}/docs/assets/readme-header.html`);
  await page.evaluate(() => document.fonts.ready);
  assert(await page.evaluate(() => document.fonts.check('72px "CND Hybrid Display"')));
  assert(await page.locator('.monogram').evaluate(img => img.complete && img.naturalWidth > 0));
  await page.locator('#header').screenshot({ path: 'docs/assets/readme-header.png' });
  console.log('Rendered README header from bundled Hybrid font and CND artwork.');
} finally {
  await browser.close();
}
