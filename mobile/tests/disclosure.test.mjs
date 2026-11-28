import assert from 'node:assert/strict';
import { test } from 'node:test';
import { setImmediate } from 'node:timers/promises';
import { bundle, mount } from '../../tests/preview.mjs';
import { fixture, now } from './fixture.mjs';

const mobile = await bundle(new URL('../src/main.ts', import.meta.url));

test('provider history keeps its disclosure and focus while time, notices and readings change', async t => {
  let time = now;
  let offline = false;
  const snapshot = fixture();
  const codex = snapshot.providers.find(provider => provider.id === 'codex');
  const claude = snapshot.providers.find(provider => provider.id === 'claude');
  codex.dailyTokens = [{ date: '2026-10-15', tokens: 123, scope: 'account' }];
  claude.dailyTokens = [{ date: '2026-10-15', tokens: 42, scope: 'local' }];
  const { document, window, tick } = await mount(t, mobile, window => {
    window.Date.now = () => time * 1000;
    window.isTauri = true;
    window.__TAURI_INTERNALS__ = { invoke: async command => {
      if (command === 'read_sample' || command === 'clear_sample') return null;
      if (command === 'live_status') return { snapshot, receivedAt: now };
      if (command === 'live_pull') { if (offline) throw new Error('Synthetic offline'); return snapshot; }
      throw new Error(`Unexpected command ${command}`);
    } };
  });
  const card = name => [...document.querySelectorAll('article')].find(article => article.querySelector('h2').textContent === name);
  const details = card('Codex').querySelector('details');
  const summary = details.querySelector('summary');
  const other = card('Claude').querySelector('details');
  details.open = true;
  summary.focus();
  const checkFocus = () => {
    assert.equal(card('Codex').querySelector('details'), details);
    assert.equal(document.activeElement, summary);
    assert.equal(details.open, true);
  };
  time += 900;
  tick();
  checkFocus();
  assert.match(card('Codex').textContent, /Stale/);
  document.dispatchEvent(new window.Event('visibilitychange'));
  checkFocus();
  assert.equal(card('Claude').querySelector('details'), other);
  assert.equal(other.open, false);

  offline = true;
  document.querySelector('#refresh').click();
  await setImmediate();
  assert.match(document.querySelector('#dashboard').textContent, /Refresh failed/);
  checkFocus();

  offline = false;
  snapshot.sequence++;
  snapshot.providers = [codex];
  codex.dailyTokens.push({ date: '2026-10-16', tokens: 456, scope: 'account' });
  document.querySelector('#refresh').click();
  await setImmediate();
  checkFocus();
  assert.match(summary.textContent, /2 days/);
  assert.match(details.textContent, /456/);
  assert.equal(other.isConnected, false);

  snapshot.sequence++;
  snapshot.providers = [claude, codex];
  codex.dailyTokens = [];
  document.querySelector('#refresh').click();
  await setImmediate();
  checkFocus();
  assert.match(details.textContent, /history unavailable/i);
  assert.equal(card('Claude').querySelector('details').open, false);
});
