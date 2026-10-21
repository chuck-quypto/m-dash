import { test } from 'node:test';
import assert from 'node:assert/strict';
import { renderDashboard } from '../src/view.ts';
import { readSnapshot } from '../src/snapshot.ts';
import { fixture, now } from './fixture.mjs';

test('version two allowances keep distinct labels, nullable periods and readable percentages', () => {
  const raw=fixture();raw.version=2;
  raw.providers[0].windows=[
    {id:'seven_day_opus',label:'7-day Opus allowance',durationMinutes:10080,remainingPercent:91.96000000000001,resetsAt:null},
    {id:'spend_limit',label:'Gateway <spending>',durationMinutes:null,remainingPercent:50,resetsAt:null}
  ];
  const html=renderDashboard({mode:'live',snapshot:readSnapshot(raw,now),receivedAt:now,error:null,refreshing:false,storageWarning:false},now);
  assert.match(html,/7-day Opus allowance/);assert.match(html,/Gateway &lt;spending&gt;/);
  assert.match(html,/92%/);assert.doesNotMatch(html,/>91\.960000[^<]*%/);
});

test('dashboard labels samples, unknown values and expired resets without interpolating HTML', () => {
  const raw = fixture(); raw.sourceName = '<img src=x onerror=alert(1)>';
  const html = renderDashboard({ mode: 'sample', snapshot: readSnapshot(raw, now), receivedAt: now, error: null, refreshing: false, storageWarning: false }, now);
  assert.match(html, /Sample data/);
  assert.match(html, /Unavailable/);
  assert.match(html, /Reset time passed/);
  assert.match(html, /73%/);
  assert.ok(!html.includes('<img src=x'));
  assert.match(html, /&lt;img/);
});
