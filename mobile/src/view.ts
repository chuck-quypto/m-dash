import type { State } from './companion.ts';
import { providerState, type Provider } from './snapshot.ts';

const names = { claude: 'Claude', codex: 'Codex', grok: 'Grok', openrouter: 'OpenRouter' };
const escape = (text: string) => text.replace(/[&<>"']/g, char => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' })[char]!);
const at = (value: number | null) => value === null ? 'Unavailable' : new Date(value * 1000).toLocaleString();
const percent = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });
const period = (minutes: number | null) => minutes === null ? 'Spending allowance' : minutes === 300 ? '5-hour' : minutes % 1440 === 0 ? `${minutes / 1440}-day` : `${minutes.toLocaleString()}-minute`;

function card(provider: Provider, now: number): string {
  const status = providerState(provider, now);
  const statusLabel = { current: 'Observed', stale: 'Stale', error: 'Provider error', unavailable: 'Unavailable' }[status];
  const windows = provider.windows.map(window => `<section class="allowance">
    <div class="metric-line"><h3>${escape(window.label ?? period(window.durationMinutes))}</h3><strong>${window.remainingPercent === null ? 'Unavailable' : `${percent.format(window.remainingPercent)}% <small>left</small>`}</strong></div>
    ${window.remainingPercent === null ? '<p class="muted">No allowance reading.</p>' : `<meter min="0" max="100" value="${window.remainingPercent}" aria-label="${names[provider.id]} ${escape(window.label ?? period(window.durationMinutes))} remaining">${percent.format(window.remainingPercent)}%</meter>`}
    <p class="muted">${window.resetsAt === null ? 'Reset time unavailable' : window.resetsAt <= now ? 'Reset time passed · waiting for a desktop observation' : `Resets ${at(window.resetsAt)}`}</p>
  </section>`).join('');
  const history = provider.dailyTokens.length ? `<details><summary>Daily tokens <span>(${provider.dailyTokens.length} days)</span></summary><ul class="history">${provider.dailyTokens.map(day => `<li><span>${day.date}<small>${day.scope === 'local' ? 'Desktop-local' : 'Account-wide'}</small></span><strong>${day.tokens.toLocaleString()}</strong></li>`).join('')}</ul></details>` : '<p class="muted history-empty">Token history unavailable</p>';
  return `<article class="provider"><div class="provider-title"><h2>${names[provider.id]}</h2><span class="badge ${status}">${statusLabel}</span></div>
    <p class="observed">Observed ${at(provider.observedAt)}</p>${windows}
    ${provider.id === 'openrouter' || provider.balanceUsd !== null ? `<section class="allowance"><h3>Credit balance</h3><strong class="balance">${provider.balanceUsd === null ? 'Unavailable' : `$${provider.balanceUsd.toFixed(2)} <small>USD</small>`}</strong></section>` : ''}${history}</article>`;
}

export function renderDashboard(state: State, now: number): string {
  const message = state.error === 'offline' ? 'Refresh failed. Showing the last saved snapshot.' : state.error === 'invalid-snapshot' ? 'Snapshot rejected. Showing the last valid snapshot.' : state.error === 'not-configured' ? 'Cloudflare is not connected in this preview. No request was sent.' : '';
  const warning = state.storageWarning ? '<p class="notice">Local storage is unavailable or invalid. Changes may not survive a restart; clear app storage if removal fails.</p>' : '';
  if (!state.snapshot && state.mode === 'live') return `${warning}${message ? `<p class="notice">${message}</p>` : ''}<section class="empty"><h2>Waiting for desktop readings</h2><p>Compare verification values and approve on desktop, then refresh. Keep desktop m-dash running.</p></section>`;
  if (!state.snapshot) return `${warning}${message ? `<p class="notice">${message}</p>` : ''}<section class="empty"><div class="empty-bars" aria-hidden="true"><i></i><i></i><i></i><i></i></div><h2>Keep your resources in view</h2><p>This Android preview is not connected to your desktop. Explore the dashboard with sample data while cloud setup is pending.</p><p class="muted">Your provider credentials and model pings stay on your computer.</p></section>`;
  return `${state.mode === 'sample' ? '<div class="sample-banner"><strong>Sample data</strong><span>No live accounts connected</span></div>' : '<div class="sample-banner"><strong>Encrypted desktop readings</strong><span>Read-only · credentials stay on desktop</span></div>'}${warning}${message ? `<p class="notice">${message}</p>` : ''}<section class="source"><strong>${escape(state.snapshot.sourceName)}</strong><span>Downloaded ${at(state.receivedAt)}</span><span>Readings retain their original observation times.</span></section><div class="providers">${state.snapshot.providers.map(provider => card(provider, now)).join('')}</div>`;
}
