// Run against the disposable emulator with an installed debug APK; no enrollment secret is needed.
const {_android}=await import(process.env.MDASH_PLAYWRIGHT_MODULE ?? 'playwright');
import {randomBytes} from 'node:crypto';
import assert from 'node:assert/strict';
const endpoint=process.env.MDASH_TEST_RELAY_URL;
assert(endpoint, 'Set MDASH_TEST_RELAY_URL to a relay you are authorized to test');
const d=(await _android.devices()).find(device=>device.serial()==='emulator-5554');
assert(d, 'Start the disposable emulator-5554 first');
const p=await(await d.webView({pkg:'net.chuckdudley.mdash.mobile'})).page();
let created=false;
try {
 await p.locator('#dashboard').waitFor();
 assert.equal(await p.evaluate(()=>window.__TAURI_INTERNALS__.invoke('live_status')),null,'Use an unpaired disposable emulator; existing pairings are preserved');
 const random=()=>randomBytes(32).toString('base64url');
 const offer={version:2,endpoint,id:random(),key:random(),readToken:random(),expiresAt:Math.floor(Date.now()/1000)+600};
 const code='mdash2:'+Buffer.from(JSON.stringify(offer)).toString('base64url');
 created=true;
 const result=await Promise.race([p.evaluate(async code=>{try{await window.__TAURI_INTERNALS__.invoke('live_pair',{code,consent:true});return 'unexpected success';}catch(e){return String(e);}},code),new Promise(resolve=>setTimeout(()=>resolve('native HTTPS command did not complete'),20000))]);
 assert.match(result,/authorization rejected/, 'Native HTTPS must complete with the relay HTTP 401, never hang or bypass TLS');
 console.log('PASS native Android TLS verifies the relay certificate and returns its expected HTTP 401');
} finally {if(created)await p.evaluate(()=>window.__TAURI_INTERNALS__.invoke('live_clear'));await d.close();}
