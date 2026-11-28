import assert from 'node:assert/strict';
import { readFile } from 'node:fs/promises';
import { test } from 'node:test';
import { setImmediate } from 'node:timers/promises';
import { bundle, mount } from './preview.mjs';

const desktop = await bundle(new URL('../src/main.ts', import.meta.url));
const fixture = await readFile(new URL('../preview/fixture.js', import.meta.url), 'utf8');
const preview = (t, setup) => mount(t, desktop, window => {
  window.eval(fixture);
  // jsdom has no showModal implementation. Honor the dialog's declared initial focus.
  window.HTMLDialogElement.prototype.showModal = function () {
    this.open = true;
    this.querySelector('[autofocus]')?.focus();
  };
  setup?.(window);
});

function onReading(window, change) {
  const bridge = window.__TAURI_INTERNALS__;
  const register = bridge.transformCallback;
  bridge.transformCallback = callback => register(event => {
    if (event.event === 'dashboard-updated') change(event.payload);
    callback(event);
  });
}

function closeDialog(dialog, choice) {
  dialog.returnValue = choice ? [...dialog.querySelectorAll('button')].find(button => button.textContent === choice).value : '';
  dialog.dispatchEvent(new dialog.ownerDocument.defaultView.Event('close'));
}

test('ledger drafts stay mounted through the timer and incoming readings, then submit once', async t => {
  const additions = [];
  const { document, window, tick } = await preview(t, window => {
    const bridge = window.__TAURI_INTERNALS__;
    const invoke = bridge.invoke;
    bridge.invoke = (command, args) => {
      if (command === 'add_ledger_mark') additions.push({ ...args });
      return invoke(command, args);
    };
    onReading(window, state => {
      state.error = 'Synthetic refresh warning';
      state.consistency.codex.marks = [];
      state.consistency.codex.limitHits = 4;
    });
  });
  document.querySelector('nav [data-page="consistency"]').click();
  const note = document.querySelector('#ledger-text-codex');
  const date = document.querySelector('#ledger-date-codex');
  const plan = document.querySelector('#ledger-plan-codex');
  const other = document.querySelector('#ledger-text-claude');
  other.value = 'Separate Claude draft';
  date.value = '2026-10-15';
  plan.checked = true;
  note.value = 'New allowance';
  note.focus();
  note.setSelectionRange(2, 6);
  tick();
  assert.equal(document.activeElement, note);
  assert.deepEqual([note.selectionStart, note.selectionEnd], [2, 6]);
  date.focus();
  window.previewUpdate();
  assert.equal(document.activeElement, date);
  assert.match(document.querySelector('main').textContent, /4 requests were refused/);
  assert.match(document.querySelector('main').textContent, /Synthetic refresh warning/);
  plan.focus();
  tick();
  assert.equal(document.activeElement, plan);
  assert.equal(document.querySelector('#ledger-text-claude'), other);
  assert.equal(other.value, 'Separate Claude draft');
  document.querySelector('[data-ledger-add="codex"]').click();
  assert.deepEqual(additions, [{ provider: 'codex', date: '2026-10-15', plan: true, text: 'New allowance' }]);
});

test('a ledger draft stays with its account when a reading switches accounts', async t => {
  const { document, window } = await preview(t, window => onReading(window, state => {
    state.consistency.codex.account = 'another-synthetic-account';
  }));
  document.querySelector('nav [data-page="consistency"]').click();
  const codex = document.querySelector('#ledger-text-codex');
  const claude = document.querySelector('#ledger-text-claude');
  codex.value = 'Previous Codex account';
  claude.value = 'Current Claude account';
  claude.focus();
  window.previewUpdate();
  assert.equal(codex.isConnected, false);
  assert.equal(document.querySelector('#ledger-text-codex').value, '');
  assert.equal(document.activeElement, claude);
  assert.equal(claude.value, 'Current Claude account');
});

test('settings keep the relay entries, password selection and unsaved checkbox across refreshes', async t => {
  let warning = null;
  const { document, window, tick } = await preview(t, window => onReading(window, state => { state.error = warning; }));
  document.querySelector('nav [data-page="connections"]').click();
  await setImmediate();
  const relay = document.querySelector('#usage-relay-endpoint');
  const enrollment = document.querySelector('#usage-relay-enrollment');
  const join = document.querySelector('#usage-relay-join');
  const logs = document.querySelector('#local-logs');
  relay.value = 'https://draft.example.test';
  enrollment.value = 'synthetic enrollment';
  join.value = 'synthetic join draft';
  logs.click();
  join.focus();
  join.setSelectionRange(3, 9);
  window.previewUpdate();
  tick();
  assert.equal(document.activeElement, join);
  assert.deepEqual([join.selectionStart, join.selectionEnd], [3, 9]);
  logs.focus();
  tick();
  assert.equal(document.activeElement, logs);
  assert.equal(logs.checked, true);
  assert.equal(document.querySelector('#usage-relay-endpoint'), relay);
  assert.equal(document.querySelector('#usage-relay-enrollment'), enrollment);
  assert.deepEqual([relay.value, enrollment.value, join.value], ['https://draft.example.test', 'synthetic enrollment', 'synthetic join draft']);
  const select = document.querySelector('#compact-mode');
  select.focus();
  warning = 'Synthetic update while choosing a window';
  window.previewUpdate();
  assert.equal(document.activeElement, select);
  assert.ok(document.querySelector('main').textContent.includes(warning));
});

test('deleting shared rows requires confirmation of the specific channel with Cancel initially focused', async t => {
  const id = 'C'.repeat(43);
  const endpoint = 'https://relay.example.test';
  const code = 'mdash-usage1:' + Buffer.from(JSON.stringify({ endpoint, id, read: 'synthetic-read', ingest: 'synthetic-write' })).toString('base64url');
  const deletions = [];
  const { document, tick } = await preview(t, window => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = async (command, args) => {
      if (command === 'usage_relay_status') return { endpoint, code, cloudSetup: 'synthetic-setup' };
      if (command === 'usage_relay_leave') { deletions.push(args.delete); return null; }
      return invoke(command, args);
    };
  });
  document.querySelector('nav [data-page="connections"]').click();
  await setImmediate();
  for (const choice of ['Cancel', null, 'Delete channel and shared rows']) {
    document.querySelector('#usage-relay-delete').click();
    assert.deepEqual(deletions, []);
    const dialog = document.querySelector('dialog[open]');
    assert.ok(dialog);
    assert.equal(document.getElementById(dialog.getAttribute('aria-labelledby')).textContent, 'Delete channel and shared rows?');
    const description = document.getElementById(dialog.getAttribute('aria-describedby')).textContent;
    assert.match(description, new RegExp(id));
    assert.ok(description.includes(endpoint));
    assert.doesNotMatch(dialog.textContent, /synthetic-(read|write|setup)|mdash-usage1:/);
    assert.equal(document.activeElement.textContent, 'Cancel');
    tick();
    assert.equal(document.activeElement.textContent, 'Cancel');
    closeDialog(dialog, choice);
    await setImmediate();
    if (choice !== 'Delete channel and shared rows') {
      assert.deepEqual(deletions, []);
      assert.equal(document.activeElement.id, 'usage-relay-delete');
    }
  }
  assert.deepEqual(deletions, [true]);
  assert.equal(document.activeElement.id, 'usage-relay-endpoint');
});

test('an unreadable channel ID blocks deletion and still allows a local disconnect', async t => {
  const leaves = [];
  const { document } = await preview(t, window => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = (command, args) => {
      if (command === 'usage_relay_status') return { endpoint: 'https://relay.example.test', code: 'mdash-usage1:bad', cloudSetup: '' };
      if (command === 'usage_relay_leave') { leaves.push(args.delete); return null; }
      return invoke(command, args);
    };
  });
  document.querySelector('nav [data-page="connections"]').click();
  await setImmediate();
  document.querySelector('#usage-relay-delete').click();
  assert.deepEqual(leaves, []);
  assert.match(document.querySelector('.action-notice').textContent, /channel ID could not be read/);
  document.querySelector('#usage-relay-leave').click();
  assert.deepEqual(leaves, [false]);
});

test('enabling automatic Claude pings still requires consent to the usage notice', async t => {
  const changes = [];
  const { document } = await preview(t, window => {
    const invoke = window.__TAURI_INTERNALS__.invoke;
    window.__TAURI_INTERNALS__.invoke = (command, args) => {
      if (command === 'set_claude_ping') { changes.push({ ...args }); return invoke('get_dashboard'); }
      return invoke(command, args);
    };
  });
  document.querySelector('nav [data-page="connections"]').click();
  await setImmediate();
  for (const choice of ['Cancel', 'Enable automatic pings']) {
    document.querySelector('#claude-ping-toggle').click();
    const dialog = document.querySelector('dialog[open]');
    assert.equal(document.activeElement.textContent, 'Cancel');
    assert.match(dialog.textContent, /may also consume paid credits/);
    assert.deepEqual(changes, []);
    closeDialog(dialog, choice);
    await setImmediate();
  }
  assert.deepEqual(changes, [{ enabled: true, confirmed: true }]);
});

test('daily allowance buttons name the day and all three values and open the matching usage log', async t => {
  const { document, window } = await preview(t);
  document.querySelector('nav [data-page="consistency"]').click();
  const chart = document.querySelector('.ledger-chart');
  assert.equal(chart.getAttribute('role'), 'group');
  assert.match(chart.getAttribute('aria-label'), /Codex · 5-hour/);
  const days = chart.querySelectorAll('button');
  assert.equal(days.length, 21);
  assert.match(days[0].getAttribute('aria-label'), /2026-10-08.*2 points.*2 explained.*0 unattributed/);
  assert.match(days[1].getAttribute('aria-label'), /2026-10-09.*3\.8 points.*3\.8 explained.*0 unattributed/);
  const weekly = document.querySelectorAll('.ledger-chart')[1].querySelector('[data-usage-day="2026-10-22"]');
  assert.match(weekly.getAttribute('aria-label'), /2026-10-22.*6 points.*2 explained.*4 unattributed/);
  assert.equal(weekly.title, weekly.getAttribute('aria-label'));
  assert.ok(days[1].dispatchEvent(new window.KeyboardEvent('keydown', { key: ' ', bubbles: true, cancelable: true })));
  days[1].click();
  assert.equal(document.querySelector('#usage-date').value, '2026-10-09');
  assert.equal(document.querySelector('#usage-provider').value, 'codex');
});
