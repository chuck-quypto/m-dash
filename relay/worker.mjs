const token = value => typeof value === 'string' && /^[A-Za-z0-9_-]{43}$/.test(value);
const reply = (status, body = {}) => Response.json(body, { status, headers: { 'Cache-Control': 'no-store', 'X-Content-Type-Options':'nosniff' } });
const digest = async value => btoa(String.fromCharCode(...new Uint8Array(await crypto.subtle.digest('SHA-256',new TextEncoder().encode(value))))).replaceAll('+','-').replaceAll('/','_').replaceAll('=','');
async function body(request) {
 if (!request.headers.get('Content-Type')?.startsWith('application/json')) throw 400;
 const reader=request.body?.getReader(); if (!reader) throw 400;
 const chunks=[];let size=0;
 try {for (;;) {const {value,done}=await reader.read();if(done)break;size+=value.length;if(size>100000)throw 413;chunks.push(value);}} finally {await reader.cancel();}
 const bytes=new Uint8Array(size);let offset=0;for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.length;}
 try{return JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(bytes));}catch{throw 400;}
}
function fields(value, keys) {if(!value || Array.isArray(value) || typeof value!=='object' || Object.keys(value).sort().join()!==keys.sort().join())throw 400;}
export default {
 async fetch(request, env) {
  try {
   if (!env.EPOCH || !token(env.ENROLLMENT_TOKEN)) return reply(503);
   const url=new URL(request.url); if(url.search || request.headers.has('Origin'))return reply(403);
   if(env.LIMITER && !(await env.LIMITER.limit({key:request.headers.get('CF-Connecting-IP') || 'unknown'})).success)return reply(429);
   const auth=request.headers.get('Authorization')?.replace(/^Bearer /,'');if(!token(auth))return reply(401);
   const authHash=await digest(auth);const now=Math.floor(Date.now()/1000);
   if(url.pathname==='/v1/devices' && request.method==='POST') {
    if(authHash!==await digest(env.ENROLLMENT_TOKEN) && (!token(env.WINDOWS_ENROLLMENT_TOKEN) || authHash!==await digest(env.WINDOWS_ENROLLMENT_TOKEN)))return reply(401);
    const b=await body(request);fields(b,['id','readHash','writeHash']);if(!token(b.id)||!token(b.readHash)||!token(b.writeHash)||b.readHash===b.writeHash)throw 400;
    await env.DB.prepare('DELETE FROM devices WHERE epoch != ? OR (approved=0 AND expires<=?)').bind(env.EPOCH,now).run();
    const result=await env.DB.prepare('INSERT OR IGNORE INTO devices (id,epoch,read_hash,write_hash,expires) SELECT ?,?,?,?,? WHERE (SELECT COUNT(*) FROM devices)<4').bind(b.id,env.EPOCH,b.readHash,b.writeHash,now+600).run();
    return reply(result.meta.changes===1?201:409);
   }
   const match=/^\/v1\/devices\/([A-Za-z0-9_-]{43})(?:\/(claim|approve|snapshot))?$/.exec(url.pathname);if(!match)return reply(404);
   const [,id,action]=match;
   const device=await env.DB.prepare('SELECT * FROM devices WHERE id=? AND epoch=?').bind(id,env.EPOCH).first();
   const writing=request.method==='DELETE' || action==='approve' || (action==='snapshot' && request.method==='PUT') || (!action && request.method==='GET');
   if(!device || authHash!==(writing?device.write_hash:action==='claim'?device.read_hash:device.reader_hash))return reply(401);
   if(request.method==='DELETE' && !action) {await env.DB.prepare('DELETE FROM devices WHERE id=? AND epoch=?').bind(id,env.EPOCH).run();return reply(200);}
   if(!device.approved && device.expires<=now)return reply(410);
   if(request.method==='GET' && !action)return reply(200,{claim:device.claim,approved:!!device.approved});
   if(request.method==='POST' && action==='claim') {
    const b=await body(request);fields(b,['claim','readHash']);if(!token(b.claim)||!token(b.readHash)||b.readHash===device.read_hash||b.readHash===device.write_hash)throw 400;
    const result=await env.DB.prepare('UPDATE devices SET claim=?,reader_hash=? WHERE id=? AND epoch=? AND approved=0 AND expires>? AND (claim IS NULL OR (claim=? AND reader_hash=?))').bind(b.claim,b.readHash,id,env.EPOCH,now,b.claim,b.readHash).run();
    return reply(result.meta.changes===1?200:409);
   }
   if(request.method==='POST' && action==='approve') {
    const b=await body(request);fields(b,['claim']);if(!token(b.claim))throw 400;
    const result=await env.DB.prepare('UPDATE devices SET approved=1 WHERE id=? AND epoch=? AND claim=? AND (expires>? OR approved=1)').bind(id,env.EPOCH,b.claim,now).run();
    return reply(result.meta.changes===1?200:409);
   }
   if(!device.approved)return reply(403);
   if(action==='snapshot' && request.method==='PUT') {
    const b=await body(request);fields(b,['version','id','sequence','publishedAt','nonce','ciphertext']);
    if(b.version!==1||b.id!==id||!Number.isSafeInteger(b.sequence)||b.sequence<1||!Number.isSafeInteger(b.publishedAt)||b.publishedAt<now-300||b.publishedAt>now+300||typeof b.nonce!=='string'||! /^[A-Za-z0-9_-]{16}$/.test(b.nonce)||typeof b.ciphertext!=='string'||b.ciphertext.length<22||b.ciphertext.length>87404||! /^[A-Za-z0-9_-]+$/.test(b.ciphertext))throw 400;
    const result=await env.DB.prepare('UPDATE devices SET sequence=?,published=?,snapshot=?,uploaded=? WHERE id=? AND epoch=? AND approved=1 AND sequence<? AND published<=?').bind(b.sequence,b.publishedAt,JSON.stringify(b),now,id,env.EPOCH,b.sequence,b.publishedAt).run();
    return reply(result.meta.changes===1?200:409);
   }
   if(action==='snapshot' && request.method==='GET') {
    if(device.uploaded>0 && device.uploaded<=now-604800)return reply(410);
    if(!device.snapshot)return reply(404);
    return new Response(device.snapshot,{headers:{'Content-Type':'application/json','Cache-Control':'no-store'}});
   }
   return reply(405);
  } catch(error) { return reply(typeof error==='number'?error:503); }
 },
 async scheduled(_event, env) {
  const now=Math.floor(Date.now()/1000);
  await env.DB.batch([
   env.DB.prepare('DELETE FROM devices WHERE epoch != ? OR (approved=0 AND expires<=?)').bind(env.EPOCH,now),
   env.DB.prepare('UPDATE devices SET snapshot=NULL WHERE uploaded>0 AND uploaded<=?').bind(now-604800)
  ]);
 }
};
