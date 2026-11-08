// Renders the burn ledger: allowance points used, the part explained by tokens observed on this
// computer, and the unattributed remainder from usage m-dash cannot see.
export interface LedgerDay { date: string; points: number; weighted: number; explained: number; unattributed: number }
export interface LedgerWindow {
  id: string; label: string; enough: boolean; activeDays: number;
  pointsPerMillion: number | null; pointsPerDollar: number | null; rateDays?: number; rateRange?: [number, number] | null;
  points: number; explained: number; unattributed: number; idlePoints: number; idleSteps: number[];
  change: { date: string; factor: number; daysAfter?: number } | null; days: LedgerDay[];
}
export interface LedgerSurface {
  surface: string; querySource: string; model: string; requests: number;
  input: number; output: number; cacheRead: number; cacheWrite: number; costUsd: number; pricedRequests?: number;
}
export interface LedgerMark { at: number; kind: 'plan' | 'note'; source: 'auto' | 'manual'; text: string }
export interface Ledger {
  marks?: LedgerMark[];
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
// Rows shared by your other computers arrive as "other:<surface>", or "other:<surface>@<name>" once that computer has announced its name.
export function surfaceName(surface: string) {
  if (!surface.startsWith('other:')) return surfaceNames[surface] ?? surface;
  const [where, ...name] = surface.slice(6).split('@');
  return `${name.length ? name.join('@') : 'Other computer'} · ${surfaceNames[where] ?? where}`;
}
const shared = (surface: string) => surface === 'cloud' || surface.startsWith('other:');

const markDate = (mark: LedgerMark) => new Date(mark.at * 1000).toISOString().slice(0, 10);
const dayNumber = (date: string) => Date.parse(`${date}T00:00:00Z`) / 86_400_000;

function dayBars(provider: 'codex' | 'claude', window: LedgerWindow, marks: LedgerMark[]) {
  const days = window.days;
  const marked = new Map<string, string[]>();
  for (const mark of marks) marked.set(markDate(mark), [...(marked.get(markDate(mark)) ?? []), mark.text]);
  const max = Math.max(...days.map(day => day.points), 1);
  return `<div class="chart ledger-chart" role="img" aria-label="${escape(window.label)}: allowance points per day, explained and unattributed. Exact values are in the copied CSV.">${days.map(day => `<div class="bar-cell${marked.has(day.date) ? ' marked' : ''}" role="button" tabindex="0" data-usage-provider="${provider}" data-usage-day="${escape(day.date)}" title="Open this day in the usage log. "${escape(day.date)}: ${number.format(day.points)} points, ${number.format(day.explained)} explained, ${number.format(day.unattributed)} unattributed${marked.has(day.date) ? ` · ${escape(marked.get(day.date)!.join(' · '))}` : ''}"><div class="ledger-stack" style="height:${Math.max(2, day.points / max * 100)}%"><div class="ledger-unattributed" style="flex:${day.unattributed}"></div><div class="ledger-explained" style="flex:${day.explained}"></div></div></div>`).join('')}</div>
    <div class="chart-axis"><span>${escape(days[0]?.date ?? '')}</span><span>${escape(days.at(-1)?.date ?? '')}</span></div>`;
}

function idleHours(steps: number[]) {
  if (!steps.length) return '';
  const hours = Array.from({ length: 24 }, () => 0);
  for (const at of steps) hours[new Date(at * 1000).getHours()] += 1;
  const max = Math.max(...hours, 1);
  return `<h4>Steps with no observed usage, by hour (your time)</h4><div class="chart ledger-hours" role="img" aria-label="Allowance steps with no observed usage by hour of day">${hours.map((count, hour) => `<div class="bar-cell" title="${hour}:00: ${count} step${count === 1 ? '' : 's'}"><div class="bar" style="height:${count ? Math.max(4, count / max * 100) : 0}%"></div></div>`).join('')}</div><div class="chart-axis"><span>0:00</span><span>12:00</span><span>23:00</span></div>`;
}

// How far to trust the learned rate: the days it rests on and the spread of the efficient days.
export function confidence(window: LedgerWindow) {
  const days = window.rateDays ?? window.activeDays;
  const range = window.rateRange;
  const wide = range ? range[1] / range[0] : 1;
  const level = days >= 14 && wide < 2 ? 'Good estimate' : days >= 7 && wide < 3 ? 'Fair estimate' : 'Rough estimate';
  const spread = range && range[1] > range[0] * 1.05 ? `, efficient days ranged ${compact.format(1e6 / range[1])}–${compact.format(1e6 / range[0])} tokens per point` : '';
  return `<span class="${level === 'Rough estimate' ? 'warning-text' : ''}">${level}: ${days} active day${days === 1 ? '' : 's'}${window.rateDays != null && window.rateDays < window.activeDays ? ' since the last plan change' : ''}${spread}.</span>`;
}

// What the allowance left means in tokens and, for Claude, API-priced usage, at the learned rate.
export function remainingWorth(ledgers: Ledgers, provider: 'codex' | 'claude', windowId: string | null, remaining: number | null) {
  const window = ledgers[provider]?.windows.find(item => item.id === windowId);
  if (!window?.enough || !window.pointsPerMillion || remaining == null || remaining <= 0) return '';
  const tokens = remaining / window.pointsPerMillion * 1e6;
  const price = provider === 'claude' && window.pointsPerDollar ? ` (about ${usd.format(remaining / window.pointsPerDollar)} of API-priced usage)` : '';
  return `≈ ${compact.format(tokens)} weighted tokens${price} left at your learned rate.`;
}

function windowCard(provider: 'codex' | 'claude', window: LedgerWindow, marks: LedgerMark[]) {
  const share = window.points > 0 ? window.unattributed / window.points : 0;
  const rate = window.pointsPerMillion == null ? '' : `${provider === 'claude' && window.pointsPerDollar != null
    ? `One point ≈ ${usd.format(1 / window.pointsPerDollar)} of API-priced usage (${compact.format(1e6 / window.pointsPerMillion)} weighted tokens).`
    : `One point ≈ ${compact.format(1e6 / window.pointsPerMillion)} weighted tokens.`} ${confidence(window)}`;
  const shift = window.change;
  const after = shift?.daysAfter;
  // A plan change or note within three days of the shift is the likely explanation.
  const near = shift ? marks.filter(mark => Math.abs(dayNumber(markDate(mark)) - dayNumber(shift.date)) <= 3) : [];
  const cause = near.length ? ` That lines up with ${near.map(mark => `<strong>${escape(mark.text)}</strong> (${escape(markDate(mark))})`).join(' and ')}.` : ' A plan change, a provider change or more usage elsewhere can all cause this. If you changed plans, add it under <strong>Plan changes and notes</strong>.';
  const change = shift ? `<p class="${near.length ? 'support' : 'warning-text'}">Around ${escape(shift.date)}, each point started matching ${shift.factor > 1 ? `about ${number.format(shift.factor)}× fewer` : `about ${number.format(1 / shift.factor)}× more`} observed tokens${after ? ` (${after} day${after === 1 ? '' : 's'} of data since)` : ''}.${cause}</p>` : '';
  return `<div class="ledger-window"><h3>${escape(window.label)}</h3>
    ${window.enough ? `<p><strong>${number.format(window.points)}</strong> points used · <strong>${number.format(window.explained)}</strong> explained by observed usage · <strong class="${share >= 0.25 ? 'warning-text' : ''}">${number.format(window.unattributed)}</strong> unattributed (${percent.format(share)})</p><p class="support">${rate}</p>${change}${dayBars(provider, window, marks)}`
      : `<p class="empty">Needs three days with both allowance movement and local usage. ${window.activeDays} so far.</p>`}
    ${window.idlePoints > 0 ? `<p class="support">${number.format(window.idlePoints)} points moved while m-dash saw no usage.</p>${idleHours(window.idleSteps)}` : ''}</div>`;
}

// Only some sources report a price: requests without one show as unknown rather than as free.
function apiPrice(row: LedgerSurface) {
  const priced = row.pricedRequests ?? (row.costUsd > 0 ? row.requests : 0);
  if (!priced) return '<span title="This source does not report a price">—</span>';
  if (priced < row.requests) return `<span title="Price reported for ${compact.format(priced)} of ${compact.format(row.requests)} requests">${usd.format(row.costUsd)}+</span>`;
  return usd.format(row.costUsd);
}

function surfaces(provider: 'codex' | 'claude', ledger: Ledger) {
  const sorted = [...ledger.surfaces].sort((a, b) => (b.input + b.output + b.cacheRead + b.cacheWrite) - (a.input + a.output + a.cacheRead + a.cacheWrite));
  // The 12 largest groups, plus every group from a cloud session or another computer, which can be small beside local use but is the point of the relay.
  const rows = [...sorted.slice(0, 12), ...sorted.slice(12).filter(row => shared(row.surface))];
  if (!rows.length) return '';
  const showCost = rows.some(row => row.costUsd > 0);
  return `<table><caption class="sr-only">Observed usage by where it ran, purpose and model</caption><thead><tr><th scope="col">Where</th><th scope="col">Purpose</th><th scope="col">Model</th><th scope="col" class="numeric">Requests</th><th scope="col" class="numeric">Input</th><th scope="col" class="numeric">Output</th><th scope="col" class="numeric">Cache read</th><th scope="col" class="numeric">Cache write</th>${showCost ? '<th scope="col" class="numeric">API price</th>' : ''}</tr></thead><tbody>${rows.map(row => `<tr class="clickable" tabindex="0" data-usage-provider="${provider}" data-usage-where="${escape(row.surface)}" data-usage-purpose="${escape(row.querySource)}" data-usage-model="${escape(row.model)}" title="Open these requests in the usage log"><td>${escape(surfaceName(row.surface))}</td><td>${escape(row.querySource || '—')}</td><td>${escape(row.model || '—')}</td><td class="numeric">${compact.format(row.requests)}</td><td class="numeric">${compact.format(row.input)}</td><td class="numeric">${compact.format(row.output)}</td><td class="numeric">${compact.format(row.cacheRead)}</td><td class="numeric">${compact.format(row.cacheWrite)}</td>${showCost ? `<td class="numeric">${apiPrice(row)}</td>` : ''}</tr>`).join('')}</tbody></table>`;
}

function serverVsLocal(ledger: Ledger) {
  const rows = ledger.serverVsLocal.slice(-Math.min(ledger.days, 28));
  const here = ledger.surfaces.some(row => shared(row.surface)) ? 'Observed' : 'This computer';
  if (!rows.length) return '';
  const server = rows.reduce((sum, row) => sum + row.server, 0);
  const local = rows.reduce((sum, row) => sum + row.local, 0);
  return `<h3>Account tokens vs observed</h3><p class="support">Codex reports account-wide daily tokens. The gap is usage m-dash did not see: cloud tasks, code review, tasks started by other agents, the IDE or computers not on your usage channel. Provider day boundaries are unspecified, so compare totals rather than single days.</p>
    <p><strong>${compact.format(server)}</strong> account · <strong>${compact.format(local)}</strong> ${here.toLowerCase()} · <strong class="${server > 0 && local / server < 0.75 ? 'warning-text' : ''}">${compact.format(Math.max(0, server - local))}</strong> elsewhere over ${rows.length} days</p>
    <table><caption class="sr-only">Codex daily tokens: account and ${here.toLowerCase()}</caption><thead><tr><th scope="col">Provider day</th><th scope="col" class="numeric">Account</th><th scope="col" class="numeric">${here}</th></tr></thead><tbody>${[...rows].reverse().map(row => `<tr><td>${escape(row.date)}</td><td class="numeric">${compact.format(row.server)}</td><td class="numeric">${compact.format(row.local)}</td></tr>`).join('')}</tbody></table>`;
}

// Plan changes m-dash saw, plus the owner's own notes. A plan change, seen or added, gives the
// days after it their own baseline rate.
function notes(provider: 'codex' | 'claude', ledger: Ledger) {
  const marks = ledger.marks ?? [];
  const today = new Date().toISOString().slice(0, 10);
  const rows = marks.length ? `<ul class="ledger-marks">${[...marks].reverse().map(mark => `<li><span class="ledger-mark-date">${escape(markDate(mark))}</span> ${escape(mark.text)}${mark.kind === 'plan' ? ' <span class="support">· plan change</span>' : ''}${mark.source === 'manual' ? ` <button class="text-button" data-ledger-remove="${provider}" data-at="${mark.at}" data-kind="${mark.kind}">Remove</button>` : ''}</li>`).join('')}</ul>` : '<p class="empty">No plan changes or notes in this range.</p>';
  return `<h3>Plan changes and notes</h3><p class="support">m-dash records plan changes it can see. Add ones it can't, such as moving between price tiers that report the same plan name, so rates before and after are learned separately. Notes only label the chart. Dates are UTC days, like the chart.</p>${rows}
    <div class="ledger-mark-form"><label>Date <input type="date" id="ledger-date-${provider}" max="${today}" value="${today}"></label><label class="grow">Note <input type="text" id="ledger-text-${provider}" maxlength="200" placeholder="Moved to the $100 plan"></label><label class="check"><input type="checkbox" id="ledger-plan-${provider}"> Plan change</label><button class="text-button" data-ledger-add="${provider}">Add</button></div>`;
}

export function ledgerCsv(ledgers: Ledgers) {
  const lines = ['provider,window,date,points,explained,unattributed,weighted_tokens'];
  for (const [provider, ledger] of Object.entries(ledgers) as [string, Ledger][])
    for (const window of ledger.windows)
      for (const day of window.days) lines.push([provider, JSON.stringify(window.label), day.date, day.points, day.explained.toFixed(3), day.unattributed.toFixed(3), day.weighted].join(','));
  return `${lines.join('\n')}\n`;
}

export const ledgerRanges = [7, 28, 90] as const;

function rangePicker(days: number) {
  return `<div class="ledger-range" role="group" aria-label="Range">${ledgerRanges.map(range => `<button class="text-button${range === days ? ' active' : ''}" data-ledger-days="${range}" aria-pressed="${range === days}">${range} days</button>`).join('')}</div>`;
}

// What the ledger's observed usage covers: this computer alone, or also the usage channel.
function observedFrom(ledgers: Ledgers) {
  const surfaces = Object.values(ledgers).flatMap(ledger => ledger?.surfaces ?? []);
  const others = surfaces.some(row => row.surface.startsWith('other:'));
  const cloud = surfaces.some(row => row.surface === 'cloud');
  if (others && cloud) return 'this computer, your other computers and Claude cloud sessions';
  if (others) return 'this computer and your other computers';
  if (cloud) return 'this computer and Claude cloud sessions';
  return 'this computer';
}

export interface Computers { name?: string; cloudLastUsage?: number | null; others?: { name: string | null; details: string | null; announcedAt: number | null; lastUsage: number | null }[] }

const ago = (seconds: number | null | undefined) => {
  if (seconds == null) return 'never';
  const hours = (Date.now() / 1000 - seconds) / 3600;
  return hours < 1 ? 'within the hour' : hours < 48 ? `${Math.round(hours)}h ago` : `${Math.round(hours / 24)} days ago`;
};
const onOff = (value: boolean | null) => value == null ? '<span class="support">unknown</span>' : value ? 'On' : '<span class="warning-text">Off</span>';
const flag = (details: string | null, name: string) => { const match = new RegExp(`(?:^|;)${name}=(\\d)`).exec(details ?? ''); return match ? match[1] === '1' : null; };

// Where the ledger's observed usage comes from, what each computer contributes and what stays invisible.
function computersPanel(computers: Computers, channel: boolean, localLogs: boolean, claudeFeed: boolean, claude: boolean) {
  const others = computers.others ?? [];
  const stale = (row: { lastUsage: number | null; announcedAt: number | null }) => Math.max(row.lastUsage ?? 0, row.announcedAt ?? 0) < Date.now() / 1000 - 3 * 86_400;
  const rows = [`<tr><td><input type="text" id="device-name" maxlength="64" value="${escape(computers.name ?? '')}" aria-label="This computer's name"> <button class="text-button" id="device-name-save">Rename</button><br><span class="support">This computer</span></td><td>Now</td><td>${onOff(localLogs)}</td><td>${claude ? onOff(claudeFeed) : '—'}</td><td></td></tr>`,
    ...others.map(row => `<tr><td>${escape(row.name ?? 'Unnamed computer')}${row.name ? '' : '<br><span class="support">Update m-dash there to see its name</span>'}</td><td class="${stale(row) ? 'warning-text' : ''}">${ago(row.lastUsage)}</td><td>${onOff(flag(row.details, 'logs'))}</td><td>${claude ? onOff(flag(row.details, 'feed')) : '—'}</td><td class="support">${escape((row.details ?? '').split(';')[0])}</td></tr>`)];
  const cloud = claude ? `<p class="support">Claude cloud sessions: last usage ${ago(computers.cloudLastUsage)}.${computers.cloudLastUsage == null ? ' If you use them, add the setup script from Settings to each cloud environment.' : ''}</p>` : '';
  return `<section class="panel"><h2>Computers and coverage</h2>
    ${channel ? '<p class="support">These computers share usage over your usage channel. Turn on <strong>Local session logs</strong> on each one so usage from while m-dash was closed is counted too. A computer whose last usage is more than three days old is shown in amber.</p>' : '<p class="support">Only this computer is counted. To include your other computers and Claude cloud sessions, set up <strong>Cloud sessions and other computers</strong> in Settings.</p>'}
    <table><caption class="sr-only">Computers reporting usage</caption><thead><tr><th scope="col">Computer</th><th scope="col">Last usage</th><th scope="col">Local session logs</th><th scope="col">Claude feed</th><th scope="col">Version</th></tr></thead><tbody>${rows.join('')}</tbody></table>
    ${channel ? cloud : ''}
    <p class="support">m-dash can't see chat and mobile apps, Claude Cowork and Duet, ChatGPT Dots, Codex cloud tasks or code review. Their use counts toward the same allowances and shows as unattributed.</p></section>`;
}

export function consistencyPage(ledgers: Ledgers, enabled: { codex: boolean; claude: boolean }, localLogs: boolean, claudeFeed: boolean, days = 28, computers: Computers = {}, channel = false) {
  const providers = (['codex', 'claude'] as const).filter(provider => enabled[provider]);
  const intro = `<p class="support">Compares how far each allowance moved over the last ${days} days with the tokens m-dash observed on ${observedFrom(ledgers)}. Token types are weighted by the provider's published prices, Claude models by their API price (an Opus token counts more than a Haiku token), and the baseline rate comes from your most efficient days, learned separately before and after each plan change. Anything above it is <strong>unattributed</strong>: usage m-dash did not see, such as chat and mobile apps, background agents such as Claude Duet, tasks started by ChatGPT Dots, Codex cloud tasks, or computers and cloud sessions not on your usage channel. A plan or provider change shows as a rate shift.</p>
    ${localLogs ? '' : '<p class="warning-text">Turn on <strong>Local session logs</strong> in Settings for Codex token detail and Claude sessions run while m-dash was closed.</p>'}
    ${enabled.claude && !claudeFeed ? '<p class="warning-text">Enable the Claude local feed in Settings for cost, cloud-session and subagent detail.</p>' : ''}`;
  if (!providers.length) return `<section class="panel"><h2>Allowance vs tokens</h2>${intro}<p class="empty">Enable Codex or Claude in Settings.</p></section>`;
  return `<section class="panel"><div class="section-heading"><h2>Allowance vs tokens</h2><div class="ledger-actions">${rangePicker(days)}<button id="copy-ledger" class="text-button">Copy daily CSV</button></div></div>${intro}</section>
    ${computersPanel(computers, channel, localLogs, claudeFeed, enabled.claude)}
    ${providers.map(provider => {
      const ledger = ledgers[provider];
      const name = provider === 'codex' ? 'Codex' : 'Claude';
      if (!ledger) return `<section class="panel"><h2>${name}</h2><p class="empty">The ledger appears after the next successful account read.</p></section>`;
      return `<section class="panel ledger"><h2>${name}</h2>
        ${ledger.windows.length ? ledger.windows.map(window => windowCard(provider, window, ledger.marks ?? [])).join('') : '<p class="empty">No allowance readings yet.</p>'}
        ${ledger.cacheReadShare != null ? `<p class="support">Cache reads were ${percent.format(ledger.cacheReadShare)} of prompt tokens. A sudden drop usually means repeated context was billed as fresh input.</p>` : ''}
        ${ledger.limitHits ? `<p class="warning-text">${ledger.limitHits} request${ledger.limitHits === 1 ? ' was' : 's were'} refused for a reached limit.</p>` : ''}
        ${provider === 'codex' ? serverVsLocal(ledger) : ''}
        ${ledger.surfaces.length ? `<h3>Observed usage</h3>${surfaces(provider, ledger)}` : '<p class="empty">No per-request usage has been recorded yet.</p>'}
        ${notes(provider, ledger)}</section>`;
    }).join('')}`;
}
