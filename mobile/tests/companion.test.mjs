import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Companion, pullCloudflare } from '../src/companion.ts';
import { fixture, now } from './fixture.mjs';
function storage() {
  const values = new Map();
  return { getItem: k => values.get(k) ?? null, setItem: (k, v) => values.set(k, v), removeItem: k => values.delete(k) };
}
test('default pull is not configured and never admits sample data as a real account', async () => {
  const app = new Companion(storage(), () => now);
  await app.refresh(pullCloudflare);
  assert.equal(app.state.mode, 'disconnected');
  assert.equal(app.state.snapshot, null);
  assert.equal(app.state.error, 'not-configured');
});

test('explicit sample survives restart and refresh failure preserves original observations', async () => {
  const cache = storage();
  const app = new Companion(cache, () => now);
  await app.useSample();
  await app.refresh(async () => fixture());
  assert.equal(app.state.mode, 'sample');
  assert.equal(app.state.snapshot.providers[0].observedAt, now - 60);
  const restored = new Companion(cache, () => now + 1200);
  assert.equal(restored.state.mode, 'sample');
  assert.equal(restored.state.snapshot.providers[0].observedAt, now - 60);
  await restored.refresh(async () => { throw new Error('private transport diagnostics'); });
  assert.equal(restored.state.error, 'offline');
  assert.equal(restored.state.snapshot.providers[0].observedAt, now - 60);
  await restored.clear();
  assert.equal(new Companion(cache, () => now).state.snapshot, null);
});

test('rollback, source changes and changed equal-sequence payloads cannot replace last good data', async () => {
  const app = new Companion(storage(), () => now + 100);
  await app.useSample();
  const good = fixture(); good.sequence = 2;
  await app.refresh(async () => good);
  for (const mutation of [s => s.sequence = 1, s => s.sourceId = 'other', s => s.sourceName = 'changed', s => { s.sequence = 3; s.publishedAt = now - 1; }]) {
    const bad = structuredClone(good); mutation(bad);
    await app.refresh(async () => bad);
    assert.equal(app.state.error, 'invalid-snapshot');
    assert.equal(app.state.snapshot.sourceName, 'Sample Mac');
    assert.equal(app.state.snapshot.sequence, 2);
  }
  await app.refresh(async () => good);
  assert.equal(app.state.error, null);
  const changedAccount = structuredClone(good); changedAccount.sequence = 3; changedAccount.providers[0].accountId = 'new-account';
  await app.refresh(async () => changedAccount);
  assert.equal(app.state.snapshot.providers[1].accountId, 'new-account');
});

test('refresh is single-flight and clearing invalidates a pending reply', async () => {
  const app = new Companion(storage(), () => now); await app.useSample();
  let resolve; let calls = 0;
  const pending = app.refresh(() => { calls++; return new Promise(r => resolve = r); });
  await app.refresh(async () => { calls++; return fixture(); });
  assert.equal(calls, 1);
  await app.clear(); await app.useSample();
  resolve(fixture()); await pending;
  assert.equal(app.state.snapshot, null);
});

test('corrupt or denied cache is recoverable and persistence failure stays visible', async () => {
  const broken = { getItem: () => '{', setItem: () => { throw new Error(); }, removeItem: () => { throw new Error(); } };
  const app = new Companion(broken, () => now);
  assert.equal(app.state.snapshot, null);
  assert.equal(app.state.storageWarning, true);
  await app.useSample(); await app.refresh(async () => fixture());
  assert.equal(app.state.snapshot.sourceName, 'Sample Mac');
  assert.equal(app.state.storageWarning, true);
  await app.clear(); assert.equal(app.state.snapshot, null);
  assert.equal(app.state.storageWarning, true);
});

test('refresh does not finish before an asynchronous cache write is durable', async () => {
  let release;
  const cache = storage();
  cache.setItem = () => new Promise(resolve => { release = resolve; });
  const app = new Companion(cache, () => now); await app.useSample();
  const refreshing = app.refresh(async () => fixture());
  await new Promise(resolve => setImmediate(resolve));
  assert.equal(app.state.refreshing, true);
  release(); await refreshing;
  assert.equal(app.state.refreshing, false);
});

test('live refresh admits validated data without writing it to the sample cache', async () => {
 const writes=[];
 const app=new Companion({getItem:()=>null,setItem:(...args)=>writes.push(args),removeItem:()=>{}},()=>now);
 await app.useLive(null,null);
 await app.refresh(async()=>fixture());
 assert.equal(app.state.mode,'live');assert.ok(app.state.snapshot);assert.equal(writes.length,0);
});
