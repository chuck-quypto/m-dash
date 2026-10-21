export type ProviderId = 'claude' | 'codex' | 'grok' | 'openrouter';
export type Window = { id: string; label?: string; durationMinutes: number | null; remainingPercent: number | null; resetsAt: number | null };
export type Day = { date: string; tokens: number; scope: 'local' | 'account' };
export type Provider = { id: ProviderId; accountId: string; observedAt: number | null; status: 'ok' | 'unavailable' | 'error'; windows: Window[]; balanceUsd: number | null; dailyTokens: Day[] };
export type Snapshot = { version: 1 | 2; sourceId: string; sourceName: string; sequence: number; publishedAt: number; providers: Provider[] };

function requireValue(ok: unknown): asserts ok {
  if (!ok) throw new Error('Invalid snapshot');
}
function record(input: unknown, keys: string[]): Record<string, unknown> {
  requireValue(input !== null && typeof input === 'object' && !Array.isArray(input));
  requireValue(Object.keys(input).length === keys.length && Object.keys(input).every(key => keys.includes(key)));
  return input as Record<string, unknown>;
}
const text = (v: unknown, max = 80) => typeof v === 'string' && v.length > 0 && v.length <= max && !/[\u0000-\u001f\u007f]/.test(v);
const integer = (v: unknown) => typeof v === 'number' && Number.isSafeInteger(v) && v >= 0;
const nullableNumber = (v: unknown, max = Number.MAX_SAFE_INTEGER) => v === null || (typeof v === 'number' && Number.isFinite(v) && v >= 0 && v <= max);
function list(input: unknown, max: number): unknown[] {
  requireValue(Array.isArray(input) && input.length <= max);
  return input;
}
function unique(values: unknown[]): void { requireValue(new Set(values).size === values.length); }

export function readSnapshot(input: unknown, now: number): Snapshot {
  try {
    requireValue(new TextEncoder().encode(JSON.stringify(input)).length <= 65536);
    const s = record(input, ['version', 'sourceId', 'sourceName', 'sequence', 'publishedAt', 'providers']);
    requireValue((s.version === 1 || s.version === 2) && text(s.sourceId) && text(s.sourceName));
    requireValue(integer(s.sequence) && Number(s.sequence) > 0 && integer(s.publishedAt) && Number(s.publishedAt) <= now + 300);
    const providers = list(s.providers, 4).map(value => {
      const p = record(value, ['id', 'accountId', 'observedAt', 'status', 'windows', 'balanceUsd', 'dailyTokens']);
      requireValue(['claude', 'codex', 'grok', 'openrouter'].includes(String(p.id)) && text(p.accountId));
      requireValue(['ok', 'error', 'unavailable'].includes(String(p.status)));
      requireValue(p.observedAt === null || (integer(p.observedAt) && Number(p.observedAt) <= Number(s.publishedAt)));
      requireValue(nullableNumber(p.balanceUsd));
      const windows = list(p.windows, 8).map(value => {
        const w = record(value, s.version === 2 ? ['id', 'label', 'durationMinutes', 'remainingPercent', 'resetsAt'] : ['id', 'durationMinutes', 'remainingPercent', 'resetsAt']);
        requireValue(text(w.id) && (s.version !== 2 || text(w.label)));
        requireValue((s.version === 2 && w.durationMinutes === null) || (integer(w.durationMinutes) && Number(w.durationMinutes) > 0));
        requireValue(nullableNumber(w.remainingPercent, 100) && (w.resetsAt === null || integer(w.resetsAt)));
        return w;
      });
      unique(windows.map(w => w.id));
      const days = list(p.dailyTokens, 30).map(value => {
        const d = record(value, ['date', 'tokens', 'scope']);
        requireValue(typeof d.date === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(d.date));
        requireValue(new Date(d.date).toISOString().slice(0, 10) === d.date);
        requireValue(integer(d.tokens) && ['local', 'account'].includes(String(d.scope)));
        return d;
      });
      unique(days.map(d => d.date));
      return p;
    });
    unique(providers.map(p => p.id));
    const snapshot = structuredClone(input) as Snapshot;
    snapshot.providers.sort((a, b) => a.id.localeCompare(b.id));
    for (const provider of snapshot.providers) {
      provider.windows.sort((a, b) => (a.durationMinutes ?? Number.MAX_SAFE_INTEGER) - (b.durationMinutes ?? Number.MAX_SAFE_INTEGER) || a.id.localeCompare(b.id));
      provider.dailyTokens.sort((a, b) => b.date.localeCompare(a.date));
    }
    return snapshot;
  } catch {
    throw new Error('Invalid snapshot');
  }
}
export function providerState(provider: Provider, now: number): 'current' | 'stale' | 'error' | 'unavailable' {
  if (provider.observedAt === null || provider.status === 'unavailable') return 'unavailable';
  if (provider.status === 'error') return 'error';
  return now - provider.observedAt >= 900 ? 'stale' : 'current';
}
