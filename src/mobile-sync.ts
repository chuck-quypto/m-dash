import { invoke, isTauri } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
export const disclosure = 'Your desktop encrypts usage snapshots for your paired phones before uploading them. The relay does not receive decryption keys or provider credentials. Active snapshots expire seven days after the last upload. Replaced or deleted encrypted snapshots may remain in Cloudflare recovery history for up to 30 additional days. Cloudflare can observe IP addresses, timing and payload sizes. Revoking a phone does not erase copies already saved on it.';
const dialog = document.createElement('dialog');
dialog.setAttribute('aria-label', 'Read-only mobile sync');
dialog.innerHTML = `<h2>Read-only mobile sync</h2><p>Share allowances, credit balances and up to 30 days of token totals. Provider credentials and pings stay on this desktop. Keep m-dash running to upload.</p><p id="sync-disclosure"></p><form id="sync-form"><label>Relay HTTPS origin<input name="endpoint" type="url" required placeholder="https://your-relay.workers.dev" autocomplete="off"></label><label>Relay enrollment secret (leave blank for this computer’s saved relay)<input name="enrollment" type="password" autocomplete="off"></label><label><input name="consent" type="checkbox" required> I understand and want to share these readings with a paired phone.</label><button type="submit">Create ten-minute pairing code</button></form><label id="code-label" hidden>Transfer privately to the Android app. Anyone with this code can request access.<textarea id="sync-code" readonly rows="5" spellcheck="false"></textarea></label><p id="sync-message" role="status"></p><div id="sync-devices"></div><button id="sync-reload">Check phone requests</button><button id="sync-close">Close</button>`;
dialog.querySelector('#sync-disclosure')!.textContent = disclosure;
document.body.append(dialog);
const message = (text: string) => { dialog.querySelector('#sync-message')!.textContent = text; };
let busy = false;
async function work(action: () => Promise<void>) {
 if (busy) return; busy = true;
 dialog.querySelectorAll<HTMLButtonElement>('button').forEach(b => b.disabled = true);
 try { await action(); } catch (error) { message(String(error)); }
 finally { busy = false; dialog.querySelectorAll<HTMLButtonElement>('button').forEach(b => b.disabled = false); }
}
type Device = { id: string; approved: boolean; revoked: boolean; needsRepair: boolean; expiresAt: number; sequence: number; verification: string | null };
async function devices() {
 const rows = await invoke<Device[]>('sync_status');
 const root = dialog.querySelector('#sync-devices')!; root.replaceChildren();
 for (const [index, row] of rows.entries()) {
  const section = document.createElement('section'); const text = document.createElement('p');
  text.textContent = `Phone ${index + 1}: ${row.revoked ? 'Removal pending; uploads stopped' : row.needsRepair ? 'Legacy pairing paused; upgrade relay and phone, revoke this pairing, then re-pair' : row.approved ? `Approved · ${row.sequence} upload attempts` : row.expiresAt <= Date.now()/1000 ? 'Code expired' : row.verification ? `Compare with your phone: ${row.verification}` : 'Waiting for phone'}`;
  section.append(text);
  if (row.verification && !row.approved && !row.revoked) {
   const approve = document.createElement('button'); approve.textContent = 'Values match — approve phone';
   approve.onclick = () => { void work(async () => { await invoke('sync_approve', { id: row.id, verification: row.verification }); message('Approved. The first encrypted upload will run within one minute.'); await devices(); }); }; section.append(approve);
  }
  const revoke = document.createElement('button'); revoke.textContent = 'Revoke & remove'; revoke.onclick = () => { void work(async () => { await invoke('sync_revoke', {id: row.id}); message('Pairing removed from relay and desktop. Existing phone copies remain on that phone.'); await devices(); }); }; section.append(revoke);root.append(section);
 }
}
dialog.querySelector<HTMLFormElement>('#sync-form')!.onsubmit = event => {
 event.preventDefault(); const form = event.currentTarget as HTMLFormElement;const data = new FormData(form);
 const enrollment = String(data.get('enrollment')); (form.elements.namedItem('enrollment') as HTMLInputElement).value = '';
 void work(async () => { const code = await invoke<string>('sync_pair', {endpoint: data.get('endpoint'), enrollment, consent: data.get('consent') === 'on'}); dialog.querySelector<HTMLTextAreaElement>('#sync-code')!.value = code; dialog.querySelector<HTMLElement>('#code-label')!.hidden = false; message('Paste this code in the installed Android app, then compare verification values and approve here.'); await devices(); });
};
dialog.querySelector<HTMLButtonElement>('#sync-reload')!.onclick = () => { void work(devices); };
dialog.querySelector<HTMLButtonElement>('#sync-close')!.onclick = () => dialog.close();
dialog.onclose = () => { dialog.querySelector<HTMLTextAreaElement>('#sync-code')!.value = ''; dialog.querySelector<HTMLElement>('#code-label')!.hidden = true; (dialog.querySelector('[name="enrollment"]') as HTMLInputElement).value = ''; };
if (isTauri()) void listen<string | null>('mobile-sync-status', event => { if (dialog.open && event.payload) message(event.payload); });
export function openMobileSync() {
 dialog.showModal(); void work(async () => {
  const endpoint=await invoke<string | null>('sync_relay');
  if(endpoint) (dialog.querySelector('[name="endpoint"]') as HTMLInputElement).value=endpoint;
  await devices();
 });
}
