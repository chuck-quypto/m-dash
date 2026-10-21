import { test } from 'node:test';
import assert from 'node:assert/strict';
import { readSnapshot, providerState } from '../src/snapshot.ts';

import { now, fixture } from './fixture.mjs';

test('snapshot admission preserves unknown allowance and sorts provider/window order', () => {
  const snapshot = readSnapshot(fixture(), now);
  assert.deepEqual(snapshot.providers.map(p => p.id), ['claude', 'codex']);
  assert.deepEqual(snapshot.providers[1].windows.map(w => w.remainingPercent), [73, null]);
});

test('untrusted snapshots reject malformed, secret-bearing, duplicate and oversized fields', () => {
  const mutations = [
    s => s.version = 2,
    s => s.apiKey = 'must-not-be-admitted',
    s => s.providers[0].windows[0].remainingPercent = -1,
    s => s.providers[0].windows[0].remainingPercent = Infinity,
    s => s.providers[0].accountId = '',
    s => s.providers.push(s.providers[0]),
    s => s.providers[0].windows.push(s.providers[0].windows[0]),
    s => s.providers[0].observedAt = now + 301,
    s => s.providers[0].observedAt = now + 1,
    s => s.sourceName = 'x'.repeat(70000),
    s => s.providers[0].dailyTokens = [{ date: '2026-02-30', tokens: 1, scope: 'local' }],
    s => s.providers[0].dailyTokens = [{ date: '2026-10-14', tokens: -1, scope: 'local' }],
    s => s.providers[0].dailyTokens = Array(31).fill({ date: '2026-10-14', tokens: 1, scope: 'local' }),
  ];
  for (const mutate of mutations) {
    const input = fixture(); mutate(input);
    assert.throws(() => readSnapshot(input, now), /Invalid snapshot/);
  }
  for (const invalid of [null, [], '{', 123]) assert.throws(() => readSnapshot(invalid, now), /Invalid snapshot/);
});

test('age and upstream failure remain visible without inventing a reset', () => {
  const provider = readSnapshot(fixture(), now).providers[1];
  assert.equal(providerState(provider, now), 'current');
  assert.equal(providerState(provider, now + 840), 'stale');
  assert.equal(provider.windows[0].remainingPercent, 73);
  assert.equal(providerState({ ...provider, status: 'error' }, now), 'error');
  assert.equal(providerState({ ...provider, observedAt: null }, now), 'unavailable');
});
