import { test } from 'node:test';
import assert from 'node:assert/strict';
import { Miniflare, convertV4MiniflareOptions } from 'miniflare';
import { readFile } from 'node:fs/promises';
import { createHash } from 'node:crypto';
import { fileURLToPath } from 'node:url';
const token = c => c.repeat(43);
const digest = s => createHash('sha256').update(s).digest('base64url');
test('a copied pairing code cannot read after the phone claims with fresh authority', async t => {
 const {request}=await fixture(t); const id=token('i'),root='/v1/devices/'+id;
 await request('/v1/devices','POST',{id,readHash:digest(token('r')),writeHash:digest(token('w'))},token('e'));
 assert.equal((await request(root+'/claim','POST',{claim:token('c'),readHash:digest(token('p'))})).status,200);
 assert.equal((await request(root+'/claim','POST',{claim:token('c'),readHash:digest(token('q'))})).status,409);
 assert.equal((await request(root+'/claim','POST',{claim:token('c'),readHash:digest(token('p'))})).status,200);
 await request(root+'/approve','POST',{claim:token('c')},token('w'));
 const envelope={version:1,id,sequence:1,publishedAt:Math.floor(Date.now()/1000),nonce:'n'.repeat(16),ciphertext:'c'.repeat(24)};
 await request(root+'/snapshot','PUT',envelope,token('w'));
 assert.equal((await request(root+'/snapshot','GET',undefined,token('r'))).status,401);
 assert.deepEqual(await (await request(root+'/snapshot','GET',undefined,token('p'))).json(),envelope);
});
async function fixture(t, epoch='test-epoch', bindings={}) {
 const mf=new Miniflare(convertV4MiniflareOptions({modules:true,scriptPath:fileURLToPath(new URL('../worker.mjs',import.meta.url)),compatibilityDate:'2026-10-17',d1Databases:{DB:'test'},bindings:{ENROLLMENT_TOKEN:token('e'),EPOCH:epoch,...bindings}}));
 t.after(()=>mf.dispose());
 const db=await mf.getD1Database('DB'); await db.exec((await readFile(new URL('../schema.sql',import.meta.url),'utf8')).replaceAll('\n',' '));
 return {mf, request:async (path,method='GET',body,auth=(path.endsWith('/snapshot') && method==='GET'?token('p'):token('r')))=>mf.dispatchFetch('https://relay.test'+path,{method,headers:{Authorization:'Bearer '+auth,'Content-Type':'application/json'},body:body===undefined?undefined:JSON.stringify(body)})};
}

test('Windows enrollment access preserves Mac access and cannot grant reader tokens enrollment', async t=>{
 const {request}=await fixture(t,'test-epoch',{WINDOWS_ENROLLMENT_TOKEN:token('s')});
 const enroll=(id,auth)=>request('/v1/devices','POST',{id:token(id),readHash:digest(token('r')),writeHash:digest(token('w'))},auth);
 assert.equal((await enroll('a',token('e'))).status,201);
 assert.equal((await enroll('b',token('s'))).status,201);
 assert.equal((await enroll('c',token('r'))).status,401);
 assert.equal((await enroll('d',token('x'))).status,401);
 const invalid=await fixture(t,'test-epoch',{WINDOWS_ENROLLMENT_TOKEN:'invalid'});
 assert.equal((await invalid.request('/v1/devices','POST',{id:token('i'),readHash:digest(token('r')),writeHash:digest(token('w'))},token('e'))).status,201);
 assert.equal((await invalid.request('/v1/devices','POST',{},token('s'))).status,401);
});
test('pairing needs desktop approval and separates phone reads from desktop writes',async t=>{
 const {request}=await fixture(t);const id=token('i');
 assert.equal((await request('/v1/devices','POST',{id,readHash:digest(token('r')),writeHash:digest(token('w'))},token('e'))).status,201);
 assert.equal((await request('/v1/devices/'+id+'/snapshot')).status,401);
 assert.equal((await request('/v1/devices/'+id+'/claim','POST',{claim:token('c'),readHash:digest(token('p'))})).status,200);
 assert.equal((await request('/v1/devices/'+id+'/approve','POST',{claim:token('c')})).status,401);
 assert.equal((await request('/v1/devices/'+id+'/approve','POST',{claim:token('c')},token('w'))).status,200);
 const envelope={version:1,id,sequence:1,publishedAt:Math.floor(Date.now()/1000),nonce:'n'.repeat(16),ciphertext:'c'.repeat(24)};
 assert.equal((await request('/v1/devices/'+id+'/snapshot','PUT',envelope)).status,401);
 assert.equal((await request('/v1/devices/'+id+'/snapshot','PUT',envelope,token('w'))).status,200);
 assert.deepEqual(await (await request('/v1/devices/'+id+'/snapshot')).json(),envelope);
 assert.equal((await request('/v1/devices/'+id,'DELETE',undefined,token('w'))).status,200);
 assert.equal((await request('/v1/devices/'+id+'/snapshot')).status,401);
});

test('expired pairing, replay, and restored old-epoch authority fail closed',async t=>{
 const {request,mf}=await fixture(t);const id=token('i'),root='/v1/devices/'+id;
 const db=await mf.getD1Database('DB');
 const create=()=>request('/v1/devices','POST',{id,readHash:digest(token('r')),writeHash:digest(token('w'))},token('e'));
 await create();
 const claims=await Promise.all([request(root+'/claim','POST',{claim:token('a'),readHash:digest(token('p'))}),request(root+'/claim','POST',{claim:token('b'),readHash:digest(token('p'))})]);
 assert.deepEqual(claims.map(r=>r.status).sort(),[200,409]);
 await db.prepare('UPDATE devices SET expires=0').run();
 assert.equal((await request(root+'/approve','POST',{claim:token('a')},token('w'))).status,410);
 await create();await request(root+'/claim','POST',{claim:token('c'),readHash:digest(token('p'))});await request(root+'/approve','POST',{claim:token('c')},token('w'));
 const envelope={version:1,id,sequence:2,publishedAt:Math.floor(Date.now()/1000),nonce:'n'.repeat(16),ciphertext:'c'.repeat(24)};
 assert.equal((await request(root+'/snapshot','PUT',envelope,token('w'))).status,200);
 assert.equal((await request(root+'/snapshot','PUT',{...envelope,sequence:1},token('w'))).status,409);
 assert.equal((await request(root+'/snapshot','PUT',{...envelope,ciphertext:'d'.repeat(24)},token('w'))).status,409);
 await db.prepare('UPDATE devices SET uploaded=1').run();
 assert.equal((await request(root+'/snapshot')).status,410);
 assert.equal((await request(root+'/snapshot')).status,410);
 await db.prepare("UPDATE devices SET epoch='restored-old-epoch',uploaded=?").bind(Math.floor(Date.now()/1000)).run();
 assert.equal((await request(root+'/snapshot')).status,401);
 assert.equal((await request(root+'/snapshot','PUT',{...envelope,sequence:3},token('w'))).status,401);
});
test('enrollment is private, bounded, and rejects oversized or secret-bearing wire objects',async t=>{
 const {request}=await fixture(t);
 assert.equal((await request('/v1/devices','POST',{},token('x'))).status,401);
 for(let i=0;i<4;i++)assert.equal((await request('/v1/devices','POST',{id:token(String(i)),readHash:digest(token('r')),writeHash:digest(token('w'))},token('e'))).status,201);
 assert.equal((await request('/v1/devices','POST',{id:token('z'),readHash:digest(token('r')),writeHash:digest(token('w'))},token('e'))).status,409);
 assert.equal((await request('/v1/devices','POST',{id:token('q'),readHash:digest(token('r')),writeHash:digest(token('w')),key:'secret'},token('e'))).status,400);
 assert.equal((await request('/v1/devices','POST',{payload:'x'.repeat(100001)},token('e'))).status,413);
});

test('scheduled expiry preserves approval before first upload and after expired ciphertext',async t=>{
 const {request,mf}=await fixture(t);const id=token('i'),root='/v1/devices/'+id;
 await request('/v1/devices','POST',{id,readHash:digest(token('r')),writeHash:digest(token('w'))},token('e'));
 await request(root+'/claim','POST',{claim:token('c'),readHash:digest(token('p'))});await request(root+'/approve','POST',{claim:token('c')},token('w'));
 const worker=await mf.getWorker();await worker.scheduled();
 const envelope={version:1,id,sequence:1,publishedAt:Math.floor(Date.now()/1000),nonce:'n'.repeat(16),ciphertext:'c'.repeat(24)};
 assert.equal((await request(root+'/snapshot','PUT',envelope,token('w'))).status,200);
 const db=await mf.getD1Database('DB');await db.prepare('UPDATE devices SET uploaded=1').run();await worker.scheduled();
 assert.equal((await request(root+'/snapshot')).status,410);
 assert.equal((await request(root+'/snapshot','PUT',{...envelope,sequence:2},token('w'))).status,200);
});

test('approval retry is idempotent after deadline but cannot approve an expired unapproved request',async t=>{
 const {request,mf}=await fixture(t);const id=token('i'),root='/v1/devices/'+id;
 await request('/v1/devices','POST',{id,readHash:digest(token('r')),writeHash:digest(token('w'))},token('e'));
 await request(root+'/claim','POST',{claim:token('c'),readHash:digest(token('p'))});await request(root+'/approve','POST',{claim:token('c')},token('w'));
 const db=await mf.getD1Database('DB');await db.prepare('UPDATE devices SET expires=0').run();
 assert.equal((await request(root+'/approve','POST',{claim:token('c')},token('w'))).status,200);
 assert.equal((await request(root+'/approve','POST',{claim:token('d')},token('w'))).status,409);
 await db.prepare('UPDATE devices SET approved=0').run();
 assert.equal((await request(root+'/approve','POST',{claim:token('c')},token('w'))).status,410);
});
