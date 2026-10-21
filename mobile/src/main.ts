import './style.css';
import mark from '../../src/brand/cnd-monogram-hybrid-transparent.png';
import { invoke, isTauri } from '@tauri-apps/api/core';
import { Companion, pullCloudflare, type Cache } from './companion.ts';
import { sampleSnapshot } from './sample.ts';
import { renderDashboard } from './view.ts';

const clock = () => Math.floor(Date.now() / 1000);
// Access can throw in restricted WebViews; surface a storage warning through the controller.
const cache: Cache = {
  getItem: key => localStorage.getItem(key),
  setItem: (key, value) => localStorage.setItem(key, value),
  removeItem: key => localStorage.removeItem(key),
};
let nativeSaved: string | null = null;
let nativeReadFailed = false;
if (isTauri()) {
  try { nativeSaved = await invoke<string | null>('read_sample'); } catch { nativeReadFailed = true; }
}
const nativeCache: Cache = {
  getItem: () => { if (nativeReadFailed) throw new Error('Sample storage unavailable'); return nativeSaved; },
  setItem: async (_key, contents) => { await invoke('write_sample', { contents }); nativeSaved = contents; },
  removeItem: async () => { await invoke('clear_sample'); nativeSaved = null; nativeReadFailed = false; },
};
const app = new Companion(isTauri() ? nativeCache : cache, clock);
let sample = app.state.snapshot;
let scenario = 'available';
let liveMessage = '';
let verification = '';
let pairingBusy = false;
let liveStorageFailed = false;
const root = document.querySelector<HTMLDivElement>('#app')!;
root.innerHTML = `<header><div class="brand"><div class="brand-mark" aria-hidden="true">M<span>-</span></div><div><strong>m-dash</strong><span>Mobile companion</span></div></div><img class="portfolio-mark" src="${mark}" alt="CND portfolio mark" width="64" height="48"></header>
<main><div class="heading"><div><p class="eyebrow">YOUR AVAILABLE CAPACITY</p><h1>Know what’s left.</h1></div><span id="connection" class="badge">Not connected</span></div>
<div class="actions"><button id="pair-toggle">Pair with desktop</button><button id="preview">Explore sample data</button><button id="refresh" class="secondary">Refresh</button><button id="clear" class="secondary" hidden>Exit preview &amp; clear</button></div>
<form id="pair-form" hidden><label>Ten-minute code from desktop Settings → Mobile companion<textarea id="pair-code" required rows="4" autocomplete="off" spellcheck="false"></textarea></label><p>Only usage readings and history are shared. Credentials and pings remain on desktop. See the retention disclosure below before pairing.</p><label><input id="pair-consent" type="checkbox" required> I understand the connection and retention disclosure and want to pair.</label><button id="pair-submit" type="submit">Request pairing</button></form><p id="live-message" role="status"></p><button id="retry-storage" hidden>Retry protected storage</button><p id="verification"></p><label id="scenario-label" hidden>Preview scenario <select id="scenario"><option value="available">Available readings</option><option value="stale">Stale desktop</option><option value="offline">Offline refresh</option><option value="unavailable">Unavailable allowance</option></select></label>
<div id="status" role="status" aria-live="polite"></div><div id="dashboard"></div>
<details class="privacy"><summary>Connection &amp; privacy</summary><p>Cloudflare is the default relay. Pair through the installed Android app; browser previews cannot access live data. Self-hosting remains planned. Refresh never asks a provider to collect data or ping a model.</p><h2>Managed-sync disclosure</h2><p>Your desktop encrypts usage snapshots for your paired phones before uploading them. The relay does not receive your decryption keys or provider credentials. It keeps the latest active snapshot and expires it after seven days without a new desktop upload. Replaced or deleted encrypted snapshots may remain in Cloudflare recovery history for up to 30 additional days. The service can observe connection metadata, including IP addresses, timing and payload sizes. Disconnecting a phone stops future delivery but does not erase data already saved on that phone.</p><p>The installed Android app protects pairing keys and cached readings using Android Keystore. Clearing this phone removes its local copy; revoke it on desktop to stop relay delivery. Self-hosted services will disclose their own retention.</p></details>
<footer>Read-only companion · Development preview<br>Opt-in encrypted sync · Publication on hold</footer></main>`;
const element = <T extends HTMLElement>(id: string) => document.getElementById(id) as T;
function render(): void {
  element('dashboard').innerHTML = renderDashboard(app.state, clock());
  element('connection').textContent = app.state.mode === 'sample' ? 'Sample data' : app.state.mode === 'live' ? 'Desktop pairing' : 'Not connected';
  element('preview').hidden = app.state.mode !== 'disconnected';
  element('pair-toggle').hidden = app.state.mode !== 'disconnected';
  element('live-message').textContent = liveMessage;
  element('verification').textContent = verification ? `Compare on desktop: ${verification}. Approve there only if both values match.` : '';
  element('clear').hidden = app.state.mode === 'disconnected' && !liveStorageFailed;
  element('retry-storage').hidden = !liveStorageFailed;
  element('clear').textContent = liveStorageFailed ? 'Clear protected connection' : app.state.mode === 'live' ? 'Disconnect & clear this phone' : 'Exit preview & clear';
  element('scenario-label').hidden = app.state.mode !== 'sample';
  element<HTMLButtonElement>('refresh').disabled = app.state.refreshing;
  element('status').textContent = app.state.refreshing ? 'Reading saved desktop snapshot…' : app.state.error ? 'Refresh needs attention. See the message below.' : '';
}
async function refresh(): Promise<void> {
  const work = app.refresh(app.state.mode === 'sample' ? async () => {
    if (scenario === 'offline') throw new Error('offline');
    return sample;
  } : app.state.mode === 'live' ? async () => {
    try { const snapshot = await invoke('live_pull'); liveMessage = ''; verification = ''; return snapshot; }
    catch (error) { liveMessage = String(error); throw error; }
  } : pullCloudflare);
  render(); await work; render();
}
element('preview').onclick = async () => { await app.useSample(); sample = sampleSnapshot(clock()); scenario = 'available'; element<HTMLSelectElement>('scenario').value = scenario; void refresh(); };
element('clear').onclick = async () => {
  try { if (app.state.mode === 'live' || liveStorageFailed) await invoke('live_clear'); liveStorageFailed = false; await app.clear(); sample = null; verification = ''; liveMessage = 'Local copy cleared. Revoke this phone on desktop to stop future delivery.'; }
  catch (error) { liveMessage = String(error); } render();
};
element('refresh').onclick = () => { void refresh(); };
element<HTMLSelectElement>('scenario').onchange = event => {
  scenario = (event.target as HTMLSelectElement).value;
  if (scenario !== 'offline') sample = sampleSnapshot(clock(), (sample?.sequence ?? 0) + 1, scenario);
  void refresh();
};
document.addEventListener('visibilitychange', () => { if (!document.hidden) render(); });
setInterval(() => { if (!document.hidden) render(); }, 30000);
render();

element('pair-toggle').onclick = () => {
 if (!isTauri()) { liveMessage = 'Pairing requires the installed Android app. This browser preview keeps using sample data.'; render(); return; }
 element('pair-form').hidden = !element('pair-form').hidden;
};
element<HTMLFormElement>('pair-form').onsubmit = async event => {
 event.preventDefault(); if (pairingBusy) return; pairingBusy = true;
 element<HTMLButtonElement>('pair-submit').disabled = true;
 const code = element<HTMLTextAreaElement>('pair-code').value;
 element<HTMLTextAreaElement>('pair-code').value = '';
 try {
  verification = await invoke<string>('live_pair', {code, consent: element<HTMLInputElement>('pair-consent').checked});
  await app.useLive(null, null); element('pair-form').hidden = true;
  liveMessage = 'Compare the verification value on desktop, approve there, then refresh here.';
 } catch (error) { liveMessage = String(error); await restoreLive(); }
 finally { pairingBusy = false; element<HTMLButtonElement>('pair-submit').disabled = false; render(); }
};
async function restoreLive() {
 if (!isTauri()) return;
 try {
  const saved = await invoke<{verification: string; snapshot: unknown; receivedAt: number | null; warning: string | null} | null>('live_status');
  liveStorageFailed = false;
  if (saved) { await app.useLive(saved.snapshot, saved.receivedAt); verification = saved.snapshot ? '' : saved.verification; if (saved.warning) liveMessage = saved.warning; }
 } catch (error) { liveStorageFailed = true; liveMessage = String(error); }
 render();
}
element('retry-storage').onclick = () => { void restoreLive(); };
await restoreLive();
