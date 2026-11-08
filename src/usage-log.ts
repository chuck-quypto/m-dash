// The usage log: every request m-dash recorded for one account, newest first, filtered and paged
// in the native store, with the allowance steps of the same span shown between the rows.
import { surfaceName, type Ledgers } from './consistency';

export interface UsageFilter { provider: 'codex' | 'claude'; days: number; date: string; where: string; model: string; purpose: string; page: number }
export interface UsageRow { at: number; where: string; model: string; purpose: string; input: number; output: number; cacheRead: number; cacheWrite: number; costMicros: number | null }
export interface UsageStep { window: string; at: number; used: number; previous: number }
export interface UsageList { rows: UsageRow[]; steps: UsageStep[]; total: number; page: number; pageSize: number; choices: { where: string[]; model: string[]; purpose: string[] } }

const escape = (value: unknown) => String(value ?? '').replace(/[&<>"']/g, char => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char]!));
const compact = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 });
const number = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });
const usd = new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD', maximumFractionDigits: 4 });
const time = (seconds: number) => new Date(seconds * 1000).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit', second: '2-digit' });

export const emptyFilter = (provider: 'codex' | 'claude', days = 28): UsageFilter => ({ provider, days, date: '', where: '', model: '', purpose: '', page: 0 });

function select(id: string, label: string, value: string, options: [string, string][]) {
  return `<label>${label} <select id="${id}">${options.map(([option, text]) => `<option value="${escape(option)}"${option === value ? ' selected' : ''}>${escape(text)}</option>`).join('')}</select></label>`;
}

export function usageLogPage(filter: UsageFilter, list: UsageList | null | undefined, error: string | null, providers: ('codex' | 'claude')[], ledgers: Ledgers) {
  if (!providers.length) return '<section class="panel"><h2>Usage log</h2><p class="empty">Enable Codex or Claude in Settings.</p></section>';
  const labels = new Map((ledgers[filter.provider]?.windows ?? []).map(window => [window.id, window.label]));
  const choices = list?.choices ?? { where: [], model: [], purpose: [] };
  const keep = (values: string[], current: string) => current && !values.includes(current) ? [...values, current] : values;
  const filters = `<div class="usage-filters">
    ${select('usage-provider', 'Account', filter.provider, providers.map(provider => [provider, provider === 'codex' ? 'Codex' : 'Claude']))}
    ${select('usage-days', 'Range', filter.date ? '' : String(filter.days), [...(filter.date ? [['', 'One day'] as [string, string]] : []), ['7', '7 days'], ['28', '28 days'], ['90', '90 days']])}
    <label>Day <input type="date" id="usage-date" value="${escape(filter.date)}"></label>
    ${select('usage-where', 'Where', filter.where, [['', 'Anywhere'], ...keep(choices.where, filter.where).map(value => [value, surfaceName(value)] as [string, string])])}
    ${select('usage-model', 'Model', filter.model, [['', 'Any model'], ...keep(choices.model, filter.model).map(value => [value, value || '—'] as [string, string])])}
    ${select('usage-purpose', 'Purpose', filter.purpose, [['', 'Any purpose'], ...keep(choices.purpose, filter.purpose).map(value => [value, value || '—'] as [string, string])])}
    <button class="text-button" id="usage-reset">Clear filters</button></div>`;
  const heading = `<div class="section-heading"><h2>Usage log</h2><button id="copy-usage" class="text-button">Copy CSV</button></div>
    <p class="support">Every request m-dash recorded for this account, newest first, with allowance steps (▲) in between. Requests carry only accounting fields: never prompts or replies. Per-request rows are kept 90 days.</p>`;
  if (error) return `<section class="panel">${heading}${filters}<p class="warning-text" role="status">${escape(error)}</p></section>`;
  if (!list) return `<section class="panel">${heading}${filters}<p class="empty">Loading…</p></section>`;
  type Entry = { at: number; html: string };
  const entries: Entry[] = list.rows.map(row => ({ at: row.at, html: `<tr><td>${escape(time(row.at))}</td><td>${escape(surfaceName(row.where))}</td><td>${escape(row.model || '—')}</td><td>${escape(row.purpose || '—')}</td><td class="numeric">${compact.format(row.input)}</td><td class="numeric">${compact.format(row.output)}</td><td class="numeric">${compact.format(row.cacheRead)}</td><td class="numeric">${compact.format(row.cacheWrite)}</td><td class="numeric">${row.costMicros == null ? '<span title="This source does not report a price">—</span>' : usd.format(row.costMicros / 1e6)}</td></tr>` }));
  // Steps only interleave on unfiltered lists: with a filter, rows that moved the allowance may be hidden.
  if (!filter.where && !filter.model && !filter.purpose)
    entries.push(...list.steps.map(step => ({ at: step.at, html: `<tr class="usage-step"><td>${escape(time(step.at))}</td><td colspan="8">▲ ${escape(labels.get(step.window) ?? step.window)} ${number.format(step.previous)}% → ${number.format(step.used)}% used</td></tr>` })));
  entries.sort((a, b) => b.at - a.at);
  const first = list.page * list.pageSize;
  const pager = list.total > list.pageSize ? `<div class="usage-pager"><button class="text-button" id="usage-prev"${list.page === 0 ? ' disabled' : ''}>← Newer</button><span>${compact.format(first + 1)}–${compact.format(Math.min(first + list.pageSize, list.total))} of ${compact.format(list.total)}</span><button class="text-button" id="usage-next"${first + list.pageSize >= list.total ? ' disabled' : ''}>Older →</button></div>` : `<p class="support">${compact.format(list.total)} request${list.total === 1 ? '' : 's'}.</p>`;
  const table = list.rows.length ? `<table class="usage-log"><caption class="sr-only">Recorded requests</caption><thead><tr><th scope="col">Time</th><th scope="col">Where</th><th scope="col">Model</th><th scope="col">Purpose</th><th scope="col" class="numeric">Input</th><th scope="col" class="numeric">Output</th><th scope="col" class="numeric">Cache read</th><th scope="col" class="numeric">Cache write</th><th scope="col" class="numeric">API price</th></tr></thead><tbody>${entries.map(entry => entry.html).join('')}</tbody></table>` : '<p class="empty">No requests match these filters.</p>';
  return `<section class="panel">${heading}${filters}${pager}${table}${list.rows.length && list.total > list.pageSize ? pager : ''}</section>`;
}
