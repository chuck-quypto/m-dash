// Renders the burn ledger: allowance points used, the part explained by tokens observed on this
// computer, and the unattributed remainder from usage m-dash cannot see.
export interface LedgerDay { date: string; points: number; weighted: number; explained: number; unattributed: number }
export interface LedgerWindow {
  id: string; label: string; enough: boolean; activeDays: number;
  pointsPerMillion: number | null; pointsPerDollar: number | null;
  points: number; explained: number; unattributed: number; idlePoints: number; idleSteps: number[];
  change: { date: string; factor: number } | null; days: LedgerDay[];
}
export interface LedgerSurface {
  surface: string; querySource: string; model: string; requests: number;
  input: number; output: number; cacheRead: number; cacheWrite: number; costUsd: number;
}
export interface Ledger {
  account: string; days: number; limitHits: number; windows: LedgerWindow[]; surfaces: LedgerSurface[];
  cacheReadShare: number | null; serverVsLocal: { date: string; server: number; local: number }[];
}
export type Ledgers = { codex?: Ledger; claude?: Ledger };

const escape = (value: unknown) => String(value ?? '').replace(/[&<>"']/g, char => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char]!));
const number = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });
const compact = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 1 });
const usd = new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD' });
const percent = new Intl.NumberFormat(undefined, { style: 'percent', maximumFractionDigits: 0 });
const surfaceNames: Record<string, string> = { cli: 'Terminal', desktop: 'Desktop app', cloud: 'Cloud session', codex: 'Codex CLI' };

function dayBars(window: LedgerWindow) {
  const days = window.days.slice(-28);
  const max = Math.max(...days.map(day => day.points), 1);
  return `<div class="chart ledger-chart" role="img" aria-label="${escape(window.label)}: allowance points per day, explained and unattributed. Exact values are in the copied CSV.">${days.map(day => `<div class="bar-cell" title="${escape(day.date)}: ${number.format(day.points)} points, ${number.format(day.explained)} explained, ${number.format(day.unattributed)} unattributed"><div class="ledger-stack" style="height:${Math.max(2, day.points / max * 100)}%"><div class="ledger-unattributed" style="flex:${day.unattributed}"></div><div class="ledger-explained" style="flex:${day.explained}"></div></div></div>`).join('')}</div>
    <div class="chart-axis"><span>${escape(days[0]?.date ?? '')}</span><span>${escape(days.at(-1)?.date ?? '')}</span></div>`;
}

function idleHours(steps: number[]) {
  if (!steps.length) return '';
  const hours = Array.from({ length: 24 }, () => 0);
  for (const at of steps) hours[new Date(at * 1000).getHours()] += 1;
  const max = Math.max(...hours, 1);
  return `<h4>Steps with no local usage, by hour (your time)</h4><div class="chart ledger-hours" role="img" aria-label="Allowance steps with no local usage by hour of day">${hours.map((count, hour) => `<div class="bar-cell" title="${hour}:00: ${count} step${count === 1 ? '' : 's'}"><div class="bar" style="height:${count ? Math.max(4, count / max * 100) : 0}%"></div></div>`).join('')}</div><div class="chart-axis"><span>0:00</span><span>12:00</span><span>23:00</span></div>`;
}

function windowCard(provider: 'codex' | 'claude', window: LedgerWindow) {
  const share = window.points > 0 ? window.unattributed / window.points : 0;
  const rate = window.pointsPerMillion == null ? '' : provider === 'claude' && window.pointsPerDollar != null
    ? `One point ≈ ${usd.format(1 / window.pointsPerDollar)} of API-priced usage (${compact.format(1e6 / window.pointsPerMillion)} weighted tokens).`
    : `One point ≈ ${compact.format(1e6 / window.pointsPerMillion)} weighted tokens.`;
  const change = window.change ? `<p class="warning-text">The rate shifted on ${escape(window.change.date)}: each point now takes ${window.change.factor > 1 ? `${number.format(window.change.factor)}× fewer` : `${number.format(1 / window.change.factor)}× more`} tokens than before.</p>` : '';
  return `<div class="ledger-window"><h3>${escape(window.label)}</h3>
    ${window.enough ? `<p><strong>${number.format(window.points)}</strong> points used · <strong>${number.format(window.explained)}</strong> explained by tokens on this computer · <strong class="${share >= 0.25 ? 'warning-text' : ''}">${number.format(window.unattributed)}</strong> unattributed (${percent.format(share)})</p><p class="support">${rate}</p>${change}${dayBars(window)}`
      : `<p class="empty">Needs three days with both allowance movement and local usage. ${window.activeDays} so far.</p>`}
    ${window.idlePoints > 0 ? `<p class="support">${number.format(window.idlePoints)} points moved while no request was seen on this computer.</p>${idleHours(window.idleSteps)}` : ''}</div>`;
}

function surfaces(ledger: Ledger) {
  const rows = [...ledger.surfaces].sort((a, b) => (b.input + b.output + b.cacheRead + b.cacheWrite) - (a.input + a.output + a.cacheRead + a.cacheWrite)).slice(0, 12);
  if (!rows.length) return '';
  const showCost = rows.some(row => row.costUsd > 0);
  return `<table><caption class="sr-only">Observed usage by where it ran, purpose and model</caption><thead><tr><th scope="col">Where</th><th scope="col">Purpose</th><th scope="col">Model</th><th scope="col" class="numeric">Requests</th><th scope="col" class="numeric">Input</th><th scope="col" class="numeric">Output</th><th scope="col" class="numeric">Cache read</th><th scope="col" class="numeric">Cache write</th>${showCost ? '<th scope="col" class="numeric">API price</th>' : ''}</tr></thead><tbody>${rows.map(row => `<tr><td>${escape(surfaceNames[row.surface] ?? row.surface)}</td><td>${escape(row.querySource || '—')}</td><td>${escape(row.model || '—')}</td><td class="numeric">${compact.format(row.requests)}</td><td class="numeric">${compact.format(row.input)}</td><td class="numeric">${compact.format(row.output)}</td><td class="numeric">${compact.format(row.cacheRead)}</td><td class="numeric">${compact.format(row.cacheWrite)}</td>${showCost ? `<td class="numeric">${usd.format(row.costUsd)}</td>` : ''}</tr>`).join('')}</tbody></table>`;
}

function serverVsLocal(ledger: Ledger) {
  const rows = ledger.serverVsLocal.slice(-14);
  if (!rows.length) return '';
  const server = rows.reduce((sum, row) => sum + row.server, 0);
  const local = rows.reduce((sum, row) => sum + row.local, 0);
  return `<h3>Account tokens vs this computer</h3><p class="support">Codex reports account-wide daily tokens. The gap is usage this computer did not log: cloud tasks, code review, tasks started by other agents, the IDE or other computers. Provider day boundaries are unspecified, so compare totals rather than single days.</p>
    <p><strong>${compact.format(server)}</strong> account · <strong>${compact.format(local)}</strong> this computer · <strong class="${server > 0 && local / server < 0.75 ? 'warning-text' : ''}">${compact.format(Math.max(0, server - local))}</strong> elsewhere over ${rows.length} days</p>
    <table><caption class="sr-only">Codex daily tokens: account and this computer</caption><thead><tr><th scope="col">Provider day</th><th scope="col" class="numeric">Account</th><th scope="col" class="numeric">This computer</th></tr></thead><tbody>${[...rows].reverse().map(row => `<tr><td>${escape(row.date)}</td><td class="numeric">${compact.format(row.server)}</td><td class="numeric">${compact.format(row.local)}</td></tr>`).join('')}</tbody></table>`;
}

export function ledgerCsv(ledgers: Ledgers) {
  const lines = ['provider,window,date,points,explained,unattributed,weighted_tokens'];
  for (const [provider, ledger] of Object.entries(ledgers) as [string, Ledger][])
    for (const window of ledger.windows)
      for (const day of window.days) lines.push([provider, JSON.stringify(window.label), day.date, day.points, day.explained.toFixed(3), day.unattributed.toFixed(3), day.weighted].join(','));
  return `${lines.join('\n')}\n`;
}

export function consistencyPage(ledgers: Ledgers, enabled: { codex: boolean; claude: boolean }, localLogs: boolean, claudeFeed: boolean) {
  const providers = (['codex', 'claude'] as const).filter(provider => enabled[provider]);
  const intro = `<p class="support">Compares how far each allowance moved with the tokens seen on this computer over the last 28 days. Token types are weighted by the provider's published prices, and the baseline rate comes from your most efficient days. Anything above it is <strong>unattributed</strong>: cloud sessions, chat and mobile apps, background agents such as Claude Duet, tasks started by ChatGPT Dots, or other computers. A provider change shows as a rate shift.</p>
    ${localLogs ? '' : '<p class="warning-text">Turn on <strong>Local session logs</strong> in Settings for Codex token detail and Claude sessions run while m-dash was closed.</p>'}
    ${enabled.claude && !claudeFeed ? '<p class="warning-text">Enable the Claude local feed in Settings for cost, cloud-session and subagent detail.</p>' : ''}`;
  if (!providers.length) return `<section class="panel"><h2>Allowance vs tokens</h2>${intro}<p class="empty">Enable Codex or Claude in Settings.</p></section>`;
  return `<section class="panel"><div class="section-heading"><h2>Allowance vs tokens</h2><button id="copy-ledger" class="text-button">Copy daily CSV</button></div>${intro}</section>
    ${providers.map(provider => {
      const ledger = ledgers[provider];
      const name = provider === 'codex' ? 'Codex' : 'Claude';
      if (!ledger) return `<section class="panel"><h2>${name}</h2><p class="empty">The ledger appears after the next successful account read.</p></section>`;
      return `<section class="panel ledger"><h2>${name}</h2>
        ${ledger.windows.length ? ledger.windows.map(window => windowCard(provider, window)).join('') : '<p class="empty">No allowance readings yet.</p>'}
        ${ledger.cacheReadShare != null ? `<p class="support">Cache reads were ${percent.format(ledger.cacheReadShare)} of prompt tokens. A sudden drop usually means repeated context was billed as fresh input.</p>` : ''}
        ${ledger.limitHits ? `<p class="warning-text">${ledger.limitHits} request${ledger.limitHits === 1 ? ' was' : 's were'} refused for a reached limit.</p>` : ''}
        ${provider === 'codex' ? serverVsLocal(ledger) : ''}
        ${ledger.surfaces.length ? `<h3>Observed usage</h3>${surfaces(ledger)}` : '<p class="empty">No per-request usage has been recorded yet.</p>'}</section>`;
    }).join('')}`;
}
