const token = value => typeof value === 'string' && /^[A-Za-z0-9_-]{43}$/.test(value);
const reply = (status, body = {}) => Response.json(body, { status, headers: { 'Cache-Control': 'no-store', 'X-Content-Type-Options':'nosniff' } });
const digest = async value => btoa(String.fromCharCode(...new Uint8Array(await crypto.subtle.digest('SHA-256',new TextEncoder().encode(value))))).replaceAll('+','-').replaceAll('/','_').replaceAll('=','');
async function body(request, limit=100000) {
 if (!request.headers.get('Content-Type')?.startsWith('application/json')) throw 400;
 const reader=request.body?.getReader(); if (!reader) throw 400;
 const chunks=[];let size=0;
 try {for (;;) {const {value,done}=await reader.read();if(done)break;size+=value.length;if(size>limit)throw 413;chunks.push(value);}} finally {await reader.cancel();}
 const bytes=new Uint8Array(size);let offset=0;for(const chunk of chunks){bytes.set(chunk,offset);offset+=chunk.length;}
 try{return JSON.parse(new TextDecoder('utf-8',{fatal:true}).decode(bytes));}catch{throw 400;}
}
function fields(value, keys) {if(!value || Array.isArray(value) || typeof value!=='object' || Object.keys(value).sort().join()!==keys.sort().join())throw 400;}
// Usage channels carry per-request accounting rows (never content) between a user's own computers
// and from Claude Code cloud sessions, whose OTLP exporter cannot reach a loopback receiver.
const USAGE_KEYS=['provider','account','key','at','surface','model','effort','speed','querySource','agent','input','output','cacheRead','cacheWrite','reasoning','costMicros','origin'];
const label=(value,max=128)=>value===null || (typeof value==='string' && value.length>0 && value.length<=max);
const amount=value=>Number.isSafeInteger(value) && value>=0;
function usageRow(row) {
 fields(row,[...USAGE_KEYS]);
 if(!['claude','codex'].includes(row.provider)||!label(row.account,300)||row.account===null||!label(row.key,300)||row.key===null||!amount(row.at)||!label(row.surface,64)||row.surface===null)throw 400;
 for(const name of ['model','effort','speed','querySource','agent'])if(!label(row[name]))throw 400;
 for(const name of ['input','output','cacheRead','cacheWrite'])if(!amount(row[name]))throw 400;
 for(const name of ['reasoning','costMicros'])if(row[name]!==null && !amount(row[name]))throw 400;
 if(row.origin!=='cloud' && !token(row.origin))throw 400;
 return row;
}
const attribute=(attrs,name)=>{const value=(Array.isArray(attrs)?attrs:[]).find(a=>a?.key===name)?.value;return value?.stringValue ?? value?.intValue ?? value?.doubleValue ?? null;};
const count=value=>{const n=typeof value==='string'?Number(value):value;return Number.isSafeInteger(n) && n>=0?n:null;};
const text=(value,max=128)=>typeof value==='string' && value.length>0 && value.length<=max?value:null;
// Keeps only api_request accounting attributes; prompts, tool data and identity fields such as email are dropped here.
function otlpRows(payload, now) {
 const rows=[];
 for(const resource of Array.isArray(payload?.resourceLogs)?payload.resourceLogs:[]) {
  if(!['claude-code','claude-code-desktop'].includes(attribute(resource?.resource?.attributes,'service.name')))continue;
  for(const scope of Array.isArray(resource.scopeLogs)?resource.scopeLogs:[]) for(const record of Array.isArray(scope?.logRecords)?scope.logRecords:[]) {
   const a=record?.attributes;
   if(attribute(a,'event.name')!=='api_request' && record?.body?.stringValue!=='claude_code.api_request')continue;
   const account=text(attribute(a,'user.account_uuid'),256),org=text(attribute(a,'organization.id'),256),request=text(attribute(a,'request_id'),256);
   const at=Math.floor(Date.parse(attribute(a,'event.timestamp'))/1000);
   const tokens=['input_tokens','output_tokens','cache_read_tokens','cache_creation_tokens'].map(name=>count(attribute(a,name)));
   if(!account||!org||!request||!Number.isSafeInteger(at)||at<0||at>now+300||tokens.includes(null))continue;
   const usd=Number(attribute(a,'cost_usd'));const micros=count(attribute(a,'cost_usd_micros')) ?? (Number.isFinite(usd) && usd>=0 && usd<1e9?Math.round(usd*1e6):null);
   rows.push({provider:'claude',account:'claude:'+JSON.stringify([account,org]),key:'claude:'+request,at,surface:'cloud',model:text(attribute(a,'model')),effort:text(attribute(a,'effort')),speed:text(attribute(a,'speed')),
    querySource:text(attribute(a,'query_source')),agent:text(attribute(a,'agent_id')),input:tokens[0],output:tokens[1],cacheRead:tokens[2],cacheWrite:tokens[3],reasoning:null,costMicros:micros,origin:'cloud'});
  }
 }
 return rows;
}
async function storeRows(env, channel, rows, now) {
 if(!rows.length)return;
 await env.DB.batch(rows.map(row=>env.DB.prepare('INSERT OR IGNORE INTO usage_rows (channel,account,key,received,row) SELECT ?,?,?,?,? WHERE (SELECT COUNT(*) FROM usage_rows WHERE channel=?)<200000').bind(channel,row.account,row.key,now,JSON.stringify(row),channel)));
}
async function usage(request, env, url, now) {
 const logs=/^\/v1\/usage\/([A-Za-z0-9_-]{43})\/([A-Za-z0-9_-]{43})\/logs$/.exec(url.pathname);
 if(logs) {
  // The OTLP exporter carries its write token in the path; header configuration differs between exporters.
  if(request.method!=='POST')return reply(405);
  const channel=await env.DB.prepare('SELECT * FROM usage_channels WHERE id=? AND epoch=?').bind(logs[1],env.EPOCH).first();
  if(!channel || await digest(logs[2])!==channel.ingest_hash)return reply(401);
  await storeRows(env,logs[1],otlpRows(await body(request,262144),now),now);
  return reply(200,{});
 }
 const auth=request.headers.get('Authorization')?.replace(/^Bearer /,'');if(!token(auth))return reply(401);
 const authHash=await digest(auth);
 if(url.pathname==='/v1/usage' && request.method==='POST') {
  if(authHash!==await digest(env.ENROLLMENT_TOKEN) && (!token(env.WINDOWS_ENROLLMENT_TOKEN) || authHash!==await digest(env.WINDOWS_ENROLLMENT_TOKEN)))return reply(401);
  const b=await body(request);fields(b,['id','ingestHash','readHash']);if(!token(b.id)||!token(b.ingestHash)||!token(b.readHash)||b.ingestHash===b.readHash)throw 400;
  await env.DB.prepare('DELETE FROM usage_channels WHERE epoch != ?').bind(env.EPOCH).run();
  const result=await env.DB.prepare('INSERT OR IGNORE INTO usage_channels (id,epoch,ingest_hash,read_hash,created) SELECT ?,?,?,?,? WHERE (SELECT COUNT(*) FROM usage_channels)<4').bind(b.id,env.EPOCH,b.ingestHash,b.readHash,now).run();
  return reply(result.meta.changes===1?201:409);
 }
 const match=/^\/v1\/usage\/([A-Za-z0-9_-]{43})(?:\/rows(?:\/(\d{1,15}))?)?$/.exec(url.pathname);if(!match)return reply(404);
 const [,id,after]=match;
 const channel=await env.DB.prepare('SELECT * FROM usage_channels WHERE id=? AND epoch=?').bind(id,env.EPOCH).first();
 if(!channel)return reply(401);
 const rows=url.pathname.endsWith('/rows');
 if(request.method==='POST' && rows) {
  if(authHash!==channel.ingest_hash)return reply(401);
  const b=await body(request);fields(b,['rows']);if(!Array.isArray(b.rows)||b.rows.length>200)throw 400;
  await storeRows(env,id,b.rows.map(usageRow),now);
  return reply(200,{});
 }
 if(authHash!==channel.read_hash)return reply(401);
 if(request.method==='GET' && after!==undefined) {
  const {results}=await env.DB.prepare('SELECT seq,row FROM usage_rows WHERE channel=? AND seq>? ORDER BY seq LIMIT 200').bind(id,Number(after)).all();
  return reply(200,{rows:results.map(r=>({seq:r.seq,...JSON.parse(r.row)}))});
 }
 if(request.method==='DELETE' && !rows && after===undefined) {
  await env.DB.batch([env.DB.prepare('DELETE FROM usage_rows WHERE channel=?').bind(id),env.DB.prepare('DELETE FROM usage_channels WHERE id=?').bind(id)]);
  return reply(200);
 }
 return reply(405);
}
export default {
 async fetch(request, env) {
  try {
   if (!env.EPOCH || !token(env.ENROLLMENT_TOKEN)) return reply(503);
   const url=new URL(request.url); if(url.search || request.headers.has('Origin'))return reply(403);
   if(env.LIMITER && !(await env.LIMITER.limit({key:request.headers.get('CF-Connecting-IP') || 'unknown'})).success)return reply(429);
   if(url.pathname==='/v1/usage' || url.pathname.startsWith('/v1/usage/'))return await usage(request,env,url,Math.floor(Date.now()/1000));
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
   env.DB.prepare('UPDATE devices SET snapshot=NULL WHERE uploaded>0 AND uploaded<=?').bind(now-604800),
   env.DB.prepare('DELETE FROM usage_rows WHERE received<=? OR channel NOT IN (SELECT id FROM usage_channels WHERE epoch=?)').bind(now-1209600,env.EPOCH),
   env.DB.prepare('DELETE FROM usage_channels WHERE epoch != ?').bind(env.EPOCH)
  ]);
 }
};
