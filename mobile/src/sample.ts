import type { Snapshot } from './snapshot.ts';

export function sampleSnapshot(now: number, sequence = 1, scenario = 'available'): Snapshot {
  const observedAt = now - (scenario === 'stale' ? 7200 : 60);
  const unavailable = scenario === 'unavailable';
  return {
    version: 1, sourceId: 'sample-desktop', sourceName: 'Sample desktop', sequence, publishedAt: now,
    providers: [
      { id: 'codex', accountId: 'sample-codex', observedAt, status: 'ok', balanceUsd: null,
        windows: [{ id: 'weekly', durationMinutes: 10080, remainingPercent: 82, resetsAt: now + 259200 }, { id: 'short', durationMinutes: 300, remainingPercent: 67, resetsAt: now + 7200 }],
        dailyTokens: [{ date: new Date(now * 1000).toISOString().slice(0, 10), tokens: 124000, scope: 'account' }] },
      { id: 'claude', accountId: 'sample-claude', observedAt: unavailable ? null : observedAt, status: unavailable ? 'unavailable' : 'ok', balanceUsd: null,
        windows: [{ id: 'weekly', durationMinutes: 10080, remainingPercent: unavailable ? null : 91, resetsAt: null }, { id: 'short', durationMinutes: 300, remainingPercent: unavailable ? null : 42, resetsAt: now - 60 }],
        dailyTokens: unavailable ? [] : [{ date: new Date(now * 1000).toISOString().slice(0, 10), tokens: 38500, scope: 'local' }] },
      { id: 'grok', accountId: 'sample-grok', observedAt, status: 'ok', balanceUsd: null,
        windows: [{ id: 'weekly', durationMinutes: 10080, remainingPercent: 76, resetsAt: now + 86400 }], dailyTokens: [] },
      { id: 'openrouter', accountId: 'sample-openrouter', observedAt, status: 'ok', balanceUsd: 18.42, windows: [], dailyTokens: [] },
    ],
  };
}
