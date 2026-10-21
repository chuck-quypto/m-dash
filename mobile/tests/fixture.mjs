export const now = 1_800_000_000;
export function fixture() {
  const provider = (id) => ({ id, accountId: `${id}-opaque`, observedAt: now - 60, status: 'ok', balanceUsd: null, dailyTokens: [], windows: [] });
  const codex = provider('codex');
  codex.windows = [
    { id: 'weekly', durationMinutes: 10080, remainingPercent: null, resetsAt: null },
    { id: 'short', durationMinutes: 300, remainingPercent: 73, resetsAt: now - 10 },
  ];
  return { version: 1, sourceId: 'sample-desktop', sourceName: 'Sample Mac', sequence: 1, publishedAt: now, providers: [codex, provider('claude')] };
}

