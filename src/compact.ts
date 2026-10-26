import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { getCurrentWindow } from '@tauri-apps/api/window';
import './compact.css';
import { alertMark, alertSentence, type Alert, type AlertReason } from './alerts';
import portfolioMark from './brand/cnd-monogram-hybrid-transparent.png';

interface Allowance {
  label: string;
  period: string;
  remainingPercent: number | null;
  resetsAt: number | null;
  observedAt: number | null;
  status: 'fresh' | 'stale' | 'unavailable' | 'resetPending' | 'restricted';
  alert: Alert | null; alertReason: AlertReason | null; runsOutAt: number | null; surgeRatio: number | null;
}

const root = document.querySelector<HTMLDivElement>('#app')!;
const number = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });
const date = new Intl.DateTimeFormat('en-US', { month: 'short', day: 'numeric' });
const clock = new Intl.DateTimeFormat('en-GB', { hour: '2-digit', minute: '2-digit', hourCycle: 'h23' });
const escape = (value: unknown) => String(value ?? '').replace(/[&<>"']/g, char => ({'&':'&amp;','<':'&lt;','>':'&gt;','"':'&quot;',"'":'&#39;'}[char]!));
const statusText = { fresh: 'Current reading', stale: 'Last known reading; waiting for a refresh', unavailable: 'Allowance not supplied', resetPending: 'Recorded reset has passed; waiting for a new reading', restricted: 'Provider reports a usage restriction' };

root.innerHTML = `<section class="strip" aria-label="Account allowances remaining"><header><span class="drag" title="Drag the strip">⠿</span><button id="open" title="Open m-dash dashboard" aria-label="Open m-dash dashboard">m-dash</button><span id="summary">· % left</span><span class="timezone">Resets · local</span><img class="portfolio-mark" src="${portfolioMark}" alt="CND monogram" width="32" height="24" draggable="false"></header><div id="allowances" class="allowances"></div></section>`;
const cells = root.querySelector<HTMLDivElement>('#allowances')!;
let rows: Allowance[] = [];

function render() {
  const zone = new Intl.DateTimeFormat(undefined, { timeZoneName: 'short' }).formatToParts(new Date()).find(part => part.type === 'timeZoneName')?.value ?? 'local';
  root.querySelector('.timezone')!.textContent = 'Resets · ' + zone;
  cells.style.gridTemplateColumns = `repeat(${Math.max(rows.length, 1)}, minmax(0, 1fr))`;
  cells.innerHTML = rows.map(row => {
    const saved = row.status === 'stale' || row.status === 'resetPending';
    const marker = saved ? '*' : '';
    const percent = row.remainingPercent == null ? '—' : `${number.format(row.remainingPercent)}%${marker}`;
    const reset = row.resetsAt == null ? 'Reset unknown' : `${date.format(row.resetsAt * 1000)} · ${clock.format(row.resetsAt * 1000)}`;
    const period = row.period === 'wk' ? 'weekly' : row.period === '5h' ? '5-hour' : row.period === 'mo' ? 'monthly' : 'allowance';
    const when = (seconds: number) => `${date.format(seconds * 1000)} ${clock.format(seconds * 1000)}`;
    const alert = alertSentence(row, when);
    const detail = `${row.label} ${period}: ${row.remainingPercent == null ? 'unavailable' : `${number.format(row.remainingPercent)}% remaining`}. ${statusText[row.status]}.${alert ? ` ${alert}` : ''} Reset: ${row.resetsAt == null ? 'not supplied' : new Date(row.resetsAt * 1000).toLocaleString()}. Observed: ${row.observedAt == null ? 'not supplied' : new Date(row.observedAt * 1000).toLocaleString()}.`;
    return `<div class="allowance ${row.status} ${row.alert ?? ''}" title="${escape(detail)}" aria-label="${escape(detail)}"><div class="label"><span>${escape(row.label)} <span class="period">${escape(row.period)}</span></span><span class="percent">${alertMark(row.alert)}${percent}</span></div><div class="meter" ${row.remainingPercent == null ? 'role="img" aria-label="Allowance unavailable"' : `role="meter" aria-label="${escape(row.label)} ${escape(row.period)} remaining" aria-valuemin="0" aria-valuemax="100" aria-valuenow="${row.remainingPercent}" aria-valuetext="${escape(detail)}"`}><span style="width:${row.remainingPercent ?? 0}%"></span></div><div class="reset">${reset}</div></div>`;
  }).join('') || '<p class="empty">Choose allowance windows in m-dash Settings.</p>';
  root.querySelector('#summary')!.textContent = rows.some(row => row.status === 'stale' || row.status === 'resetPending') ? '· % left (* saved)' : '· % left';
}

root.querySelector('header')!.addEventListener('mousedown', event => {
  if (event.button !== 0 || (event.target as Element).closest('button')) return;
  event.preventDefault();
  void getCurrentWindow().startDragging().catch(() => { root.querySelector('#summary')!.textContent = '· move unavailable'; });
});
root.querySelector('#open')!.addEventListener('click', () => {
  void invoke('open_dashboard').catch(() => { root.querySelector('#summary')!.textContent = '· use tray to reopen'; });
});
render();
async function start() {
  try {
    await listen<Allowance[]>('compact-updated', event => { rows = event.payload; render(); });
    rows = await invoke<Allowance[]>('get_compact_dashboard');
    render();
  } catch {
    root.querySelector('#summary')!.textContent = '· reader unavailable';
  }
}
void start();
