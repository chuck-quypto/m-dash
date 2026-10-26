import { invoke, isTauri } from '@tauri-apps/api/core';
import { getVersion } from '@tauri-apps/api/app';
import { listen } from '@tauri-apps/api/event';
import './style.css';
import { openMobileSync } from './mobile-sync';
import { alertMark, alertSentence, windowAlert, type Alert, type AlertReason } from './alerts';
import brandMark from './brand/cnd-monogram-hybrid-transparent.png';

interface Allowance {
  id: string; label: string; remainingPercent: number; usedPercent: number;
  durationMins: number | null; resetsAt: number | null; blocked: boolean | null;
}
interface Snapshot {
  observedAt: number; plan: string | null; ordinaryUsageAllowed: boolean | null;
  windows: Allowance[];
  credits: { id: string; label: string; balance: string | null; unlimited: boolean }[];
  availableResets: number | null;
  resetDetails: { title: string | null; expiresAt: number | null }[];
  lifetimeTokens: number | null; peakDailyTokens: number | null;
  dailyUsage: { date: string; tokens: number }[];
  usageError: string | null;
}
interface Dashboard {
  settings: Settings; needsSetup: boolean;
  snapshot: Snapshot | null; error: string | null; refreshing: boolean;
  lastAttempt: number | null; nextRefresh: number | null;
  observations: number;
  grok: GrokSnapshot | null; grokError: string | null;
  claude: ClaudeSnapshot | null; claudeError: string | null; claudeUsageError: string | null; claudeEnabled: boolean;
  openrouter: OpenRouterSnapshot | null; openrouterError: string | null; openrouterConfigured: boolean;
  claudePing: { enabled: boolean; message: string; lastAttempt: number | null; lastTokens: number | null };
  forecasts: Forecast[];
}
interface Forecast {
  provider: string; label: string; unit: string; observedAt: number;
  perHour: number | null; exhaustsAt: number | null; resetsAt: number | null;
  resetBeforeExhaustion: boolean; sampleCount: number; sampleMinutes: number; reason: string | null;
  windowId: string | null; alert: Alert | null; alertReason: AlertReason | null; runsOutAt: number | null; surgeRatio: number | null;
}
type Provider = 'codex' | 'claude' | 'grok' | 'openrouter';
type CompactWindow = 'codexFiveHour' | 'codexWeekly' | 'claudeFiveHour' | 'claudeWeekly' | 'grokAllowance';
interface Settings {
  providers: Record<Provider, boolean>;
  compactWindows: CompactWindow[] | null;
}
const providerNames: Record<Provider, string> = { codex: 'Codex', claude: 'Claude', grok: 'Grok', openrouter: 'OpenRouter' };
const compactChoices: { id: CompactWindow; provider: Provider; label: string }[] = [
  { id: 'claudeFiveHour', provider: 'claude', label: 'Claude · five-hour' },
  { id: 'claudeWeekly', provider: 'claude', label: 'Claude · weekly' },
  { id: 'codexFiveHour', provider: 'codex', label: 'Codex · five-hour' },
  { id: 'codexWeekly', provider: 'codex', label: 'Codex · weekly' },
  { id: 'grokAllowance', provider: 'grok', label: 'Grok · included allowance' },
];
interface OpenRouterSnapshot {
  observedAt: number; balanceUsd: number; totalCreditsUsd: number; totalUsageUsd: number;
  dailyUsage: { date: string; tokens: number }[]; usageError: string | null;
  importedTokens: number | null; historySince: string | null;
}
interface ClaudeSnapshot {
  observedAt: number; quotaObservedAt: number | null; windows: Allowance[];
  source: string;
  observedTokens: number | null; observedSince: number | null; lastTokenAt: number | null;
  dailyUsage: { date: string; tokens: number }[];
}
interface GrokSnapshot {
  observedAt: number; plan: string | null; remainingPercent: number | null;
  prepaidCents: number | null; onDemandCapCents: number | null; onDemandUsedCents: number | null;
  periodEnd: string | null;
}

const root = document.querySelector<HTMLDivElement>('#app')!;
let state: Dashboard = { settings: { providers: { codex: false, claude: false, grok: false, openrouter: false }, compactWindows: null }, needsSetup: false, snapshot: null, error: null, refreshing: false, lastAttempt: null, nextRefresh: null, observations: 0, grok: null, grokError: null, claude: null, claudeError: null, claudeUsageError: null, claudeEnabled: false, claudePing: { enabled: false, message: "Off. No automatic model requests.", lastAttempt: null, lastTokens: null }, openrouter: null, openrouterError: null, openrouterConfigured: false, forecasts: [] };
let page: 'overview' | 'history' | 'connections' = 'overview';
let historyProvider: 'codex' | 'claude' | 'openrouter' = 'codex';
let setupBusy = false;
let setupMessage = '';
let appError = '';
const collectorUnavailable = 'The local collector could not be reached. Restart m-dash and try again.';
let settingsDraft: Settings | null = null;
let started = false;
let appVersion = '';
const number = new Intl.NumberFormat(undefined, { maximumFractionDigits: 2 });
const compact = new Intl.NumberFormat(undefined, { notation: 'compact', maximumFractionDigits: 2 });
const usd = new Intl.NumberFormat(undefined, { style: 'currency', currency: 'USD' });
const escape = (value: unknown) => String(value ?? '').replace(/[&<>"']/g, char => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char]!));
const metric = (value: number | null | undefined, short = false) => value == null ? 'Unavailable' : (short ? compact : number).format(value);
const dateTime = (seconds: number | null) => seconds == null ? 'Not supplied' : new Date(seconds * 1000).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' });
function age(seconds: number) {
  const minutes = Math.max(0, Math.floor((Date.now() / 1000 - seconds) / 60));
  return minutes < 1 ? 'just now' : minutes < 60 ? `${minutes}m ago` : `${Math.floor(minutes / 60)}h ${minutes % 60}m ago`;
}
function windowName(window: Allowance) {
  const duration = window.durationMins;
  return duration == null ? window.label : `${window.label} · ${duration % 1440 === 0 ? `${duration / 1440}-day` : `${number.format(duration / 60)}-hour`} allowance`;
}
// Marker after the value and a sentence under it; stale readings keep the plain look.
function alertParts(provider: string, windowId: string | null, remaining: number | null, resetsAt: number | null, restricted: boolean, stale: boolean) {
  const facts = stale ? null : windowAlert(provider, windowId, remaining, resetsAt, restricted, state.forecasts);
  const sentence = facts ? alertSentence(facts, dateTime) : '';
  return { alert: facts?.alert ?? null, mark: alertMark(facts?.alert ?? null), line: sentence ? `<p class="alert-line" role="status">${alertMark(facts!.alert)} ${escape(sentence)}</p>` : '' };
}
function openrouterCard() {
  const snapshot = state.openrouter;
  const stale = !state.openrouterConfigured || state.openrouterError != null || (snapshot && Date.now() / 1000 - snapshot.observedAt > 360);
  return `<article class="provider muted-card"><div class="provider-heading"><h3>OpenRouter</h3><span>Prepaid credits</span></div>
    ${snapshot ? `<span class="resource-label">Account balance</span><p class="unavailable">${usd.format(snapshot.balanceUsd)}</p><p class="connection-state ${stale ? 'warning-text' : ''}">${stale ? 'Last known balance' : 'OpenRouter credits API'} · ${age(snapshot.observedAt)}</p><p class="support">${metric(snapshot.importedTokens, true)} imported tokens since ${escape(snapshot.historySince ?? 'first import')}. Completed UTC days; today is not supplied.</p>` : '<p class="unavailable">Not connected</p><p class="support">Connect a management key in the native secure prompt to read account credit and token reports.</p>'}
    ${state.openrouterError || snapshot?.usageError ? `<p class="warning-text" role="status">${escape(state.openrouterError || snapshot?.usageError)}</p>` : ''}
    <button class="text-button" data-page="connections">Manage connection →</button></article>`;
}
function claudeCard() {
  const snapshot = state.claude;
  const windows = snapshot?.windows.filter(window => window.resetsAt != null && window.resetsAt > Date.now() / 1000) ?? [];
  const nativeUsage = snapshot?.source === 'Claude Code account usage';
  const stale = (nativeUsage ? state.claudeUsageError != null : !state.claudeEnabled || state.claudeError != null) || !snapshot?.quotaObservedAt || Date.now() / 1000 - snapshot.quotaObservedAt > 360;
  return `<article class="provider muted-card"><div class="provider-heading"><h3>Claude</h3><span>Account allowance + local tokens</span></div>
    ${windows.length ? `<div class="grok-metrics">${windows.map(window => { const alert = alertParts('Claude', window.id, window.remainingPercent, window.resetsAt, false, stale); return `<div><span class="resource-label">${escape(window.label)}</span><p class="unavailable">${metric(window.remainingPercent)}% left${alert.mark}</p><p class="support">Resets ${dateTime(window.resetsAt)}</p>${alert.line}</div>`; }).join('')}</div><p class="connection-state ${stale ? 'warning-text' : ''}">${stale ? 'Last known quota' : escape(snapshot?.source)} · ${snapshot?.quotaObservedAt ? age(snapshot.quotaObservedAt) : 'time unavailable'}</p>` : '<p class="unavailable">Quota unavailable</p>'}
    <p class="support">Shared Claude allowance, read through the signed-in Claude CLI every five minutes.</p>
    ${snapshot?.observedTokens != null ? `<p class="support"><strong>${metric(snapshot.observedTokens, true)}</strong> local tokens observed since ${dateTime(snapshot.observedSince)}.</p>` : ''}
    ${snapshot?.lastTokenAt != null ? `<p class="connection-state">Last token event · ${age(snapshot.lastTokenAt)}</p>` : ''}
    <p class="support">${state.claudeEnabled ? 'Updates arrive during Claude Code activity. Restart Claude Code after m-dash restarts to reconnect its token feed. Tokens cover this device while m-dash is running; other-device use is not included.' : 'Enable the reversible local feed in Settings. Your Claude sign-in stays with Claude Code.'}</p>
    ${state.claudeError ? `<p class="warning-text" role="status">${escape(state.claudeError)}</p>` : ''}
    ${state.claudeUsageError ? `<p class="warning-text" role="status">${escape(state.claudeUsageError)}</p>` : ''}
    <button class="text-button" data-page="connections">${state.claudeEnabled ? 'Manage feed' : 'Set up Claude'} →</button></article>`;
}
function grokCard() {
  const snapshot = state.grok;
  const stale = snapshot && (state.grokError != null || Date.now() / 1000 - snapshot.observedAt > 360);
  const grokAlert = alertParts('Grok', null, snapshot?.remainingPercent ?? null, snapshot?.periodEnd ? Date.parse(snapshot.periodEnd) / 1000 : null, false, !!stale);
  return `<article class="provider muted-card grok-card"><div class="provider-heading"><h3>Grok</h3><span>${escape(snapshot?.plan ?? 'CLI subscription')}</span></div>
    ${snapshot ? `<div class="grok-metrics"><div><span class="resource-label">Included allowance</span><p class="unavailable">${snapshot.remainingPercent == null ? 'Unavailable' : `${metric(snapshot.remainingPercent)}% left${grokAlert.mark}`}</p>${grokAlert.line}</div><div><span class="resource-label">Prepaid balance</span><p class="unavailable">${snapshot.prepaidCents == null ? 'Unavailable' : usd.format(snapshot.prepaidCents / 100)}</p></div></div>
    <p class="connection-state ${stale ? 'warning-text' : ''}">${stale ? 'Last known values' : 'Grok CLI billing'} · ${age(snapshot.observedAt)}</p>
    <p class="support">Usage period ends ${snapshot.periodEnd ? escape(new Date(snapshot.periodEnd).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' })) : 'at an unspecified time'}.</p>
    <p class="support">${snapshot.remainingPercent == null ? 'The CLI did not supply an allowance percentage. ' : ''}Prepaid balance is separate from included usage. Token history and refill dates are not supplied.</p>` : '<p class="unavailable">Not connected</p><p class="support">Reads your Grok CLI sign-in and billing. Run grok login if sign-in is needed, then refresh.</p>'}
    ${state.grokError ? `<p class="warning-text" role="status">${escape(state.grokError)}</p>` : ''}</article>`;
}
function historyBars(snapshot: Snapshot) {
  const days = snapshot.dailyUsage.slice(-21);
  if (!days.length) return '<p class="empty">Daily history has not been supplied by Codex.</p>';
  const max = Math.max(...days.map(day => day.tokens), 1);
  return `<div class="chart" role="img" aria-label="Daily Codex token history. Exact values are available in the Daily history tab.">${days.map(day => `<div class="bar-cell"><div class="bar" style="height:${Math.max(2, day.tokens / max * 100)}%" title="${escape(day.date)}: ${number.format(day.tokens)} tokens"></div></div>`).join('')}</div><div class="chart-axis"><span>${escape(days[0].date)}</span><span>${escape(days.at(-1)!.date)}</span></div>`;
}
function outlook() {
  return `<section class="panel"><h2>Capacity outlook</h2><p class="support">Recent observed pace · estimates assume that pace continues.</p>${state.forecasts.length ? state.forecasts.map(forecast => {
    const stale = Date.now() / 1000 - forecast.observedAt > 360;
    const unavailable = stale || forecast.perHour == null;
    const rate = forecast.unit === 'USD' ? usd.format(forecast.perHour ?? 0) : `${metric(forecast.perHour)} ${forecast.unit}`;
    const detail = stale ? 'Latest reading is stale; waiting for fresh activity.' : forecast.reason ?? (forecast.resetBeforeExhaustion ? `Reset comes first: ${dateTime(forecast.resetsAt)}.` : `Estimated exhaustion ${dateTime(forecast.exhaustsAt)}.`);
    return `<div class="forecast-row"><strong>${escape(forecast.provider)} · ${escape(forecast.label)}</strong><span class="forecast-rate">${unavailable ? 'Estimate unavailable' : `${rate} / hour`}</span><p class="support">${escape(detail)}</p>${unavailable ? '' : `<p class="footnote">${forecast.sampleCount} readings over ${forecast.sampleMinutes} minutes · observed ${age(forecast.observedAt)}</p>`}</div>`;
  }).join('') : '<p class="empty">Connect an account to start observing resource use.</p>'}<p class="footnote">Requires 3 readings spanning 15 minutes, using up to 6 hours of history. Resets, top-ups, plan changes, corrections and gaps over 15 minutes start a new baseline. Prepaid credit estimates exclude included allowances.</p></section>`;
}
function overview() {
  const enabled = state.settings.providers;
  if (!Object.values(enabled).some(Boolean)) return '<section class="panel"><h2>Choose the accounts you use.</h2><p class="empty">Collection is paused. Enable a provider in Settings to start monitoring its allowances or credits.</p><button class="refresh" data-page="connections">Open Settings</button></section>';
  const snapshot = state.snapshot;
  const stale = snapshot && (state.error != null || Date.now() / 1000 - snapshot.observedAt > 360);
  const status = snapshot ? stale ? 'Last known values' : 'Provider reported' : 'Waiting for account data';
  return `${enabled.codex ? `<section class="provider primary-card" aria-labelledby="codex-heading">
    <div class="provider-heading"><div><div class="eyebrow">CONNECTED THROUGH CODEX CLI</div><h2 id="codex-heading">Codex <span>${escape(snapshot?.plan ?? '')}</span></h2></div><span class="badge ${stale ? 'warning' : ''}">${status}</span></div>
    ${snapshot ? `
      <div class="resources">${snapshot.windows.length ? snapshot.windows.map(window => { const alert = alertParts('Codex', window.id, window.remainingPercent, window.resetsAt, window.blocked === true || snapshot.ordinaryUsageAllowed === false, !!stale); return `<div class="allowance"><div class="resource-label">${escape(windowName(window))}</div><div class="resource-value">${metric(window.remainingPercent)}<span>% left</span>${alert.mark}</div><meter class="${alert.alert ? `alert-${alert.alert}` : ''}" min="0" max="100" value="${window.remainingPercent}" aria-label="${escape(window.label)} remaining">${window.remainingPercent}%</meter><div class="support">${metric(window.usedPercent)}% used · resets ${dateTime(window.resetsAt)}</div>${window.blocked ? '<p class="warning-text">Provider reports a spending restriction</p>' : ''}${alert.line}</div>`; }).join('') : '<p class="empty">No allowance window was supplied.</p>'}
      <div class="resource"><div class="resource-label">Available resets</div><div class="resource-value">${metric(snapshot.availableResets)}</div><div class="support">${snapshot.resetDetails.some(row => row.expiresAt != null) ? `Next expiry ${dateTime(Math.min(...snapshot.resetDetails.flatMap(row => row.expiresAt == null ? [] : [row.expiresAt])))}` : 'Expiry details not supplied'}</div></div>
      ${snapshot.credits.length ? snapshot.credits.map(credit => `<div class="resource"><div class="resource-label">${escape(credit.label)} credits</div><div class="resource-value credits-value">${credit.unlimited ? 'Unlimited' : credit.balance == null ? 'Unavailable' : metric(Number(credit.balance))}</div><div class="support">Provider credit units · refill date not supplied</div></div>`).join('') : '<div class="resource"><div class="resource-label">Credits</div><div class="resource-value credits-value">Unavailable</div><div class="support">No credit balance supplied</div></div>'}</div>
      <div class="provider-footer"><span class="${snapshot.ordinaryUsageAllowed === false ? 'warning-text' : ''}">${snapshot.ordinaryUsageAllowed === true ? 'Included usage is available' : snapshot.ordinaryUsageAllowed === false ? 'Included usage is currently restricted' : 'Usage permission not supplied'}</span><span>Observed ${age(snapshot.observedAt)}</span></div>
    ` : `<p class="empty">${state.refreshing ? 'Reading your Codex account…' : 'Refresh to read your existing Codex sign-in. No model requests are made.'}</p>`}
  </section>` : ''}
  <div class="secondary-providers">${enabled.claude ? claudeCard() : ''}${enabled.grok ? grokCard() : ''}${enabled.openrouter ? openrouterCard() : ''}</div>
  <div class="detail-grid">${enabled.codex ? `<section class="panel"><div class="section-heading"><h2>Token activity</h2><button class="text-button" data-page="history">View daily history →</button></div>${snapshot ? `<div class="totals"><div><span class="resource-label">Account lifetime</span><strong title="${metric(snapshot.lifetimeTokens)}">${metric(snapshot.lifetimeTokens, true)}</strong></div><div><span class="resource-label">Peak reported day</span><strong title="${metric(snapshot.peakDailyTokens)}">${metric(snapshot.peakDailyTokens, true)}</strong></div></div>${historyBars(snapshot)}` : '<p class="empty">Token history will appear after a successful account read.</p>'}<p class="footnote">Codex account totals. Provider day labels are preserved; day timezone and reporting delay are not specified.</p>${snapshot?.usageError ? `<p class="warning-text">${escape(snapshot.usageError)}</p>` : ''}</section>` : ''}
  ${outlook()}</div>`;
}
function history() {
  const sources = (['codex', 'claude', 'openrouter'] as const).filter(provider => state.settings.providers[provider]);
  if (!sources.length) return '<section class="panel"><h2>Daily token history</h2><p class="empty">No enabled provider supplies token history. Saved history is kept when you turn an account off.</p><button class="text-button" data-page="connections">Choose providers in Settings →</button></section>';
  if (!sources.includes(historyProvider)) historyProvider = sources[0];
  const snapshot = historyProvider === 'codex' ? state.snapshot : historyProvider === 'claude' ? state.claude : state.openrouter;
  const description = historyProvider === 'codex' ? 'Provider-reported account day buckets, retained across refreshes. Provider day labels are preserved; the newest day can be incomplete or delayed.' : historyProvider === 'claude' ? 'Local Claude Code tokens collected while this dashboard was running, grouped by UTC day. Includes input, output, cache reads and cache creation. This is observed history, not account lifetime usage.' : 'Account token reports for completed UTC days. Today is unavailable. Imports update the previous 30 days and retain older days. Input plus output; cache and reasoning subsets are not counted again. Replacing the management key starts separate history.';
  return `<section class="panel"><div class="section-heading"><h2>Daily token history</h2><select id="history-provider" aria-label="Token history source">${sources.map(value => `<option value="${value}" ${historyProvider === value ? 'selected' : ''}>${providerNames[value]}${value === 'claude' ? ' · local observed' : value === 'openrouter' ? ' · imported' : ' account'}</option>`).join('')}</select></div><p class="support">${description}</p>${snapshot?.dailyUsage.length ? `<table><caption class="sr-only">${historyProvider} daily token totals</caption><thead><tr><th scope="col">${historyProvider === 'codex' ? 'Provider day' : 'UTC day'}</th><th scope="col" class="numeric">Tokens</th></tr></thead><tbody>${[...snapshot.dailyUsage].reverse().map(day => `<tr><td>${escape(day.date)}</td><td class="numeric">${metric(day.tokens)}</td></tr>`).join('')}</tbody></table>` : '<p class="empty">No daily history has been reported.</p>'}</section>`;
}
function connections() {
  settingsDraft ??= structuredClone(state.settings);
  const draft = settingsDraft;
  const enabled = state.settings.providers;
  const ping = state.claudePing;
  const descriptions: Record<Provider, string> = {
    codex: 'Signed-in Codex CLI · plan and allowance windows detected automatically',
    claude: 'Signed-in native Claude Code CLI · account allowances and optional local tokens',
    grok: 'Signed-in Grok CLI · included allowance and prepaid balance when supplied',
    openrouter: 'Management key · prepaid account credits and imported token history',
  };
  return `<section class="panel settings-panel"><h2>${state.needsSetup ? 'Welcome. Choose the accounts you use.' : 'Settings'}</h2>
    <p class="support">Monitor any combination of accounts. Turning a provider off pauses collection and hides its cards; saved history and credentials stay on this device.</p>
    <form id="preferences"><fieldset ${setupBusy ? 'disabled' : ''}><legend>Providers to monitor</legend><div class="provider-options">${(Object.keys(providerNames) as Provider[]).map(provider => `<label class="setting-choice"><input id="provider-${provider}" type="checkbox" data-provider="${provider}" ${draft.providers[provider] ? 'checked' : ''}><span><strong>${providerNames[provider]}</strong><small>${descriptions[provider]}</small></span></label>`).join('')}</div></fieldset>
    <fieldset ${setupBusy ? 'disabled' : ''}><legend>Tray icon and compact strip</legend><label class="setting-mode" for="compact-mode">Allowance windows <select id="compact-mode"><option value="auto" ${draft.compactWindows == null ? 'selected' : ''}>Choose automatically</option><option value="custom" ${draft.compactWindows != null ? 'selected' : ''}>Choose up to four</option></select></label>
    <p class="support">Automatic uses reported windows, prioritizing one per enabled provider, then weekly windows, up to four bars. All reported windows remain in the full dashboard. OpenRouter credits appear in the dashboard.</p>
    ${draft.compactWindows == null ? '' : `<div class="compact-options">${compactChoices.filter(choice => draft.providers[choice.provider]).map(choice => `<label class="setting-choice"><input id="window-${choice.id}" type="checkbox" data-window="${choice.id}" ${draft.compactWindows!.includes(choice.id) ? 'checked' : ''}><span>${choice.label}</span></label>`).join('') || '<p class="support">Select a provider with allowances above.</p>'}</div><p class="footnote">A selected window stays unavailable if your provider does not supply it. Limits are read from your account; no Plus/Pro setting is needed.</p>`}</fieldset>
    <div class="settings-actions"><button id="save-settings" class="refresh" type="submit" ${setupBusy ? 'disabled' : ''}>${setupBusy ? 'Saving…' : state.needsSetup ? 'Save and continue' : 'Save settings'}</button><button id="discard-settings" class="text-button" type="button" ${setupBusy ? 'disabled' : ''}>Revert changes</button><span class="support">Changes apply after saving.</span></div></form></section>
    <section class="panel settings-panel"><h2>Mobile companion</h2><p>Opt-in encrypted sharing with Android. Credentials and pings stay here.</p><button id="mobile-sync" class="refresh">Set up mobile sync</button></section>
    <section class="panel settings-panel"><h2>Automatic window start</h2>
    <div class="connection-row"><div><h3>Claude ping</h3><p>Start an idle five-hour window with one <strong>ping</strong> to <strong>Haiku 5.5 at low effort</strong>. Off by default. This consumes usage; review the notice before enabling.</p><p>Runs while m-dash is open, including in the menu bar. Use on only one computer per account. Turning off stops future requests; a request already submitted may finish.</p><p role="status">${escape(ping.message)}${ping.lastAttempt ? ` Last attempt: ${dateTime(ping.lastAttempt)}.${ping.lastTokens != null ? ` CLI-reported tokens: ${metric(ping.lastTokens)}.` : ''}` : ''}</p></div><button id="claude-ping-toggle" class="refresh" role="switch" aria-checked="${ping.enabled}" aria-label="Automatic Claude ping" ${setupBusy || (!enabled.claude && !ping.enabled) ? 'disabled' : ''}>${ping.enabled ? 'On · turn off' : 'Off · enable…'}</button></div>
    <p class="support">Codex automatic pings are pending account testing and cannot be enabled.</p></section>
    ${Object.values(enabled).some(Boolean) ? `<section class="panel settings-panel"><h2>Account connections</h2><p class="support">Sign in through each provider’s own CLI. m-dash does not buy credits or redeem resets. Model requests occur only if you separately enable Claude ping.</p>
    ${enabled.codex ? `<div class="connection-row"><div><h3>Codex CLI</h3><p>Install Codex CLI and sign in with your subscription, then refresh. Plus, Pro and other plan names and limits come from the account response. No subscription credentials are copied into m-dash.</p>${state.error ? `<p class="warning-text">${escape(state.error)}</p>` : ''}</div><span class="badge ${state.error ? 'warning' : ''}">${state.error ? 'Needs attention' : state.snapshot ? `Detected: ${escape(state.snapshot.plan ?? 'account')}` : 'Awaiting read'}</span></div>` : ''}
    ${enabled.claude ? `<div class="connection-row"><div><h3>Claude</h3><p>Account allowances are read through your signed-in native Claude CLI every five minutes. This integration uses an experimental Claude contract; a CLI change may require an m-dash update.</p><p>${state.claudeEnabled ? 'Local token feed enabled. Restart Claude Code after setup or restarting m-dash, and keep m-dash running during ordinary work.' : 'Enable the optional local feed for observed token history. Existing custom statuslines or telemetry are preserved and may require manual composition.'}</p><p>Disabling the local feed restores the settings m-dash added and keeps history. Turning off the Claude provider also stops recording local events, but leaves the feed setup in place.</p>${state.claudeUsageError ? `<p class="warning-text">Account usage: ${escape(state.claudeUsageError)}</p>` : ''}${state.claudeError ? `<p class="warning-text">Local feed: ${escape(state.claudeError)}</p>` : ''}</div><button id="claude-toggle" class="refresh" ${setupBusy ? 'disabled' : ''}>${state.claudeEnabled ? 'Disable local feed' : 'Enable local feed'}</button></div>` : ''}
    ${enabled.grok ? `<div class="connection-row"><div><h3>Grok CLI</h3><p>Install the Grok CLI and run grok login, then refresh. Your sign-in supplies plan and billing information; missing allowance percentages stay unavailable.</p>${state.grokError ? `<p class="warning-text">${escape(state.grokError)}</p>` : ''}</div><span class="badge ${state.grokError ? 'warning' : ''}">${state.grokError ? 'Needs attention' : state.grok ? 'Billing connected' : 'Awaiting read'}</span></div>` : ''}
    ${enabled.openrouter ? `<div class="connection-row"><div><h3>OpenRouter</h3><p>${state.openrouterConfigured ? 'Management key saved in your system’s secure credential store.' : 'Account-wide reports require an OpenRouter management key.'} Enter the key in the native secure prompt. The key never enters this web interface. Management keys can administer API keys and budgets; m-dash calls only the read-only credit and activity endpoints.</p><p>Disconnect removes the saved key and keeps local history. Replacing the key starts separate history. <button id="openrouter-settings" class="text-button">OpenRouter management keys ↗</button></p>${state.openrouterError ? `<p class="warning-text">${escape(state.openrouterError)}</p>` : ''}</div><div class="connection-actions"><button id="openrouter-connect" class="refresh" ${setupBusy ? 'disabled' : ''}>${state.openrouterConfigured ? 'Replace key' : 'Connect key'}</button>${state.openrouterConfigured ? `<button id="openrouter-disconnect" class="text-button" ${setupBusy ? 'disabled' : ''}>Disconnect</button>` : ''}</div></div>` : ''}</section>` : ''}`;
}
function render() {
  const focused = document.activeElement as HTMLElement | null;
  const focusId = focused?.id;
  const focusPage = focused?.dataset.page;
  const enabledCount = Object.values(state.settings.providers).filter(Boolean).length;
  root.innerHTML = `<header><div class="brand"><div class="brand-mark" aria-hidden="true">M<span>-</span></div><div><strong>m-dash</strong><span>AI resource monitor</span></div></div><div class="header-end"><div class="header-status"><span class="status-dot"></span>Local to this device</div><img class="portfolio-mark" src="${brandMark}" alt="CND monogram" width="84" height="63"></div></header>
    <main><div class="title-row"><div><p class="eyebrow">YOUR AVAILABLE CAPACITY</p><h1>Know what’s left.</h1><p class="intro">Your AI allowances, credits, and resets. Sources always in view.</p></div><button id="refresh" class="refresh" ${state.refreshing || !enabledCount ? 'disabled' : ''}>${state.refreshing ? 'Refreshing…' : 'Refresh accounts'}</button></div>
    <nav aria-label="Dashboard views">${(['overview', 'history', 'connections'] as const).map(tab => `<button data-page="${tab}" class="tab ${page === tab ? 'active' : ''}" aria-current="${page === tab ? 'page' : 'false'}">${tab === 'history' ? 'Daily history' : tab === 'connections' ? 'Settings' : 'Overview'}</button>`).join('')}<span>${enabledCount ? `${enabledCount} provider${enabledCount === 1 ? '' : 's'} enabled` : 'Collection paused'}</span></nav>
    ${appError ? `<div class="error" role="alert">${escape(appError)}</div>` : ''}
    ${state.settings.providers.codex && state.error ? `<div class="error" role="status"><strong>Couldn’t refresh Codex.</strong> ${escape(state.error)}${state.snapshot ? ' Showing the last successful observation.' : ''}</div>` : ''}
    ${setupMessage ? `<div class="error" role="status">${escape(setupMessage)}</div>` : ''}
    ${page === 'overview' ? overview() : page === 'history' ? history() : connections()}
    <footer><span>m-dash ${escape(appVersion)} · Use the tray to open, refresh or quit</span><span>${state.nextRefresh ? `Next refresh ${dateTime(state.nextRefresh)}` : enabledCount ? 'Connect an account to collect readings' : 'Collection paused'}</span></footer></main>`;
  root.querySelector<HTMLButtonElement>('#refresh')!.addEventListener('click', () => { void refresh(); });
  root.querySelectorAll<HTMLButtonElement>('[data-page]').forEach(button => button.addEventListener('click', () => { page = button.dataset.page as typeof page; render(); }));
  root.querySelector<HTMLSelectElement>('#history-provider')?.addEventListener('change', event => { historyProvider = (event.target as HTMLSelectElement).value as typeof historyProvider; render(); });
  root.querySelector<HTMLButtonElement>('#mobile-sync')?.addEventListener('click', openMobileSync);
  root.querySelector<HTMLButtonElement>('#claude-ping-toggle')?.addEventListener('click', () => { void toggleClaudePing(); });
  root.querySelector<HTMLButtonElement>('#claude-toggle')?.addEventListener('click', () => { void toggleClaude(); });
  root.querySelector<HTMLButtonElement>('#openrouter-settings')?.addEventListener('click', () => { void invoke('open_openrouter_settings').catch(error => { setupMessage = String(error); render(); }); });
  root.querySelector<HTMLButtonElement>('#openrouter-connect')?.addEventListener('click', () => { void setupOpenRouter(true); });
  root.querySelector<HTMLButtonElement>('#openrouter-disconnect')?.addEventListener('click', () => { void setupOpenRouter(false); });
  root.querySelector<HTMLFormElement>('#preferences')?.addEventListener('submit', event => { event.preventDefault(); void savePreferences(); });
  root.querySelector<HTMLButtonElement>('#discard-settings')?.addEventListener('click', () => { settingsDraft = structuredClone(state.settings); setupMessage = ''; render(); });
  root.querySelectorAll<HTMLInputElement>('[data-provider]').forEach(input => input.addEventListener('change', () => {
    const draft = settingsDraft!;
    draft.providers[input.dataset.provider as Provider] = input.checked;
    if (draft.compactWindows) draft.compactWindows = draft.compactWindows.filter(id => draft.providers[compactChoices.find(choice => choice.id === id)!.provider]);
    render();
  }));
  root.querySelector<HTMLSelectElement>('#compact-mode')?.addEventListener('change', event => {
    settingsDraft!.compactWindows = (event.target as HTMLSelectElement).value === 'auto' ? null : compactChoices.filter(choice => settingsDraft!.providers[choice.provider]).slice(0, 4).map(choice => choice.id);
    render();
  });
  root.querySelectorAll<HTMLInputElement>('[data-window]').forEach(input => input.addEventListener('change', () => {
    const selected = settingsDraft!.compactWindows!;
    const id = input.dataset.window as CompactWindow;
    if (input.checked && selected.length >= 4) { setupMessage = 'Choose at most four allowance windows. Uncheck one before adding another.'; }
    else { settingsDraft!.compactWindows = input.checked ? [...selected, id] : selected.filter(window => window !== id); setupMessage = ''; }
    render();
  }));
  if (focusId) document.getElementById(focusId)?.focus({ preventScroll: true });
  else if (focusPage) root.querySelector<HTMLButtonElement>(`[data-page="${focusPage}"]`)?.focus({ preventScroll: true });
}
async function savePreferences() {
  if (!settingsDraft) return;
  setupBusy = true; setupMessage = ''; render();
  try {
    state = await invoke<Dashboard>('save_settings', { settings: settingsDraft });
    settingsDraft = null;
    setupMessage = Object.values(state.settings.providers).some(Boolean)
      ? 'Settings saved. Enabled accounts will refresh automatically; saved history is retained.'
      : 'Settings saved. Collection is paused; saved history is retained.';
  } catch (error) { setupMessage = typeof error === 'string' ? error : 'Settings could not be saved. Your previous choices were retained.'; }
  setupBusy = false; render();
}
function confirmClaudePing(): Promise<boolean> {
  const dialog = document.createElement('dialog');
  dialog.className = 'ping-confirm';
  dialog.setAttribute('aria-labelledby', 'ping-confirm-title');
  dialog.innerHTML = `<h2 id="ping-confirm-title">Enable automatic Claude pings?</h2>
    <p>When an eligible five-hour window is idle, m-dash will send the prompt <strong>ping</strong> through your signed-in Claude Code CLI using <strong>Haiku 5.5 at low effort</strong>.</p>
    <p><strong>This consumes subscription usage, including applicable weekly allowance.</strong> The CLI adds context, so the request uses more than the word “ping” and its exact token cost is not fixed.</p>
    <p><strong>If extra usage is enabled with Claude, it may also consume paid credits.</strong> Starting a window does not refill your allowance immediately or guarantee capacity later.</p>
    <p>This applies to your current Claude account on this computer. Enable it on only one computer per account. You can turn it off at any time.</p>
    <form method="dialog" class="settings-actions"><button value="cancel" autofocus>Cancel</button><button value="enable" class="refresh">Enable automatic pings</button></form>`;
  document.body.append(dialog);
  return new Promise(resolve => {
    dialog.addEventListener('close', () => {
      const confirmed = dialog.returnValue === 'enable';
      dialog.remove();
      document.getElementById('claude-ping-toggle')?.focus();
      resolve(confirmed);
    }, { once: true });
    dialog.showModal();
  });
}
async function toggleClaudePing() {
  const enabled = !state.claudePing.enabled;
  if (enabled && !(await confirmClaudePing())) return;
  setupBusy = true; setupMessage = ''; render();
  try {
    state = await invoke<Dashboard>('set_claude_ping', { enabled, confirmed: enabled });
    setupMessage = enabled ? 'Claude ping enabled for this account. It will check fresh allowances before sending.' : 'Claude ping is off. No future automatic requests will be sent.';
  } catch (error) { setupMessage = typeof error === 'string' ? error : 'Claude ping could not be changed.'; }
  setupBusy = false; render();
}
async function toggleClaude() {
  setupBusy = true; setupMessage = ''; render();
  try { state = await invoke<Dashboard>('set_claude_enabled', { enabled: !state.claudeEnabled }); }
  catch (error) { setupMessage = typeof error === 'string' ? error : 'Claude setup could not be changed. Your existing settings were preserved.'; }
  setupBusy = false; render();
}
async function setupOpenRouter(connect: boolean) {
  setupBusy = true; setupMessage = ''; render();
  try { state = await invoke<Dashboard>(connect ? 'connect_openrouter' : 'disconnect_openrouter'); }
  catch (error) { setupMessage = typeof error === 'string' ? error : 'OpenRouter setup could not finish.'; }
  setupBusy = false; render();
}
async function refresh() {
  state.refreshing = true; render();
  try { state = await invoke<Dashboard>('refresh_dashboard'); appError = ''; }
  catch { state.refreshing = false; appError = collectorUnavailable; }
  render();
}
render();
async function start() {
  if (!isTauri()) {
    appError = 'Open the installed desktop application to connect to your accounts. Browser previews cannot read account data.';
    render(); return;
  }
  try {
    await listen<Dashboard>('dashboard-updated', event => { state = event.payload; appError = ''; if (document.activeElement?.tagName !== 'SELECT' && !root.matches(':active')) render(); });
    appVersion = await getVersion().catch(() => '');
    state = await invoke<Dashboard>('get_dashboard');
    appError = '';
    if (!started && state.needsSetup) page = 'connections';
    started = true; render();
  } catch { appError = collectorUnavailable; render(); }
}
void start();
setInterval(() => { if (!document.hidden && document.activeElement?.tagName !== 'SELECT' && !root.matches(':active')) render(); }, 30_000);
