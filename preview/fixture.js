const callbacks = new Map();
window.isTauri = true;
let nextCallback = 1;
const listeners = new Map();
const now = () => Math.floor(Date.now()/1000);
let settings = {providers:{codex:true,claude:true,grok:true,openrouter:true},compactWindows:null,localLogs:false};
const state = () => ({
  settings,needsSetup:false,
  claudePing:{enabled:false,message:'Off. No automatic model requests.',lastAttempt:null,lastTokens:null},
  refreshing:false,lastAttempt:now(),nextRefresh:Object.values(settings.providers).some(Boolean)?now()+60:null,observations:3,
  error:null,grokError:null,claudeError:null,claudeUsageError:null,claudeEnabled:false,openrouterError:null,openrouterConfigured:false,consistency,usageRelayError:null,forecasts:[
    {provider:'Codex',label:'Codex',unit:'percentage points',observedAt:now(),perHour:12,exhaustsAt:now()+6000,resetsAt:now()+7200,resetBeforeExhaustion:false,sampleCount:8,sampleMinutes:40,reason:null,windowId:'codex/primary',alert:'critical',alertReason:'pace',runsOutAt:now()+2400,surgeRatio:null},
    {provider:'Codex',label:'Codex',unit:'percentage points',observedAt:now(),perHour:2,exhaustsAt:now()+117000,resetsAt:now()+172800,resetBeforeExhaustion:false,sampleCount:12,sampleMinutes:120,reason:null,windowId:'codex/secondary',alert:'surge',alertReason:'surge',runsOutAt:now()+117000,surgeRatio:2.4},
    {provider:'Claude',label:'Five-hour allowance',unit:'percentage points',observedAt:now(),perHour:30,exhaustsAt:now()+5400,resetsAt:now()+4000,resetBeforeExhaustion:true,sampleCount:6,sampleMinutes:30,reason:null,windowId:'five_hour',alert:'warning',alertReason:'pace',runsOutAt:now()+3600,surgeRatio:null}
  ],
  snapshot:{observedAt:now(),plan:'plus',ordinaryUsageAllowed:true,windows:[
    {id:'codex/primary',label:'Codex',durationMins:300,remainingPercent:20,usedPercent:80,resetsAt:now()+7200,blocked:false},
    {id:'codex/secondary',label:'Codex',durationMins:10080,remainingPercent:65,usedPercent:35,resetsAt:now()+172800,blocked:false}
  ],credits:[],availableResets:null,resetDetails:[],lifetimeTokens:1200000,peakDailyTokens:50000,dailyUsage:[{date:'2026-10-12',tokens:20000},{date:'2026-10-13',tokens:50000}],usageError:null},
  claude:{observedAt:now(),quotaObservedAt:now(),source:'Claude Code account usage',windows:[{id:'five_hour',label:'Five-hour allowance',durationMins:300,remainingPercent:45,usedPercent:55,resetsAt:now()+4000}],observedTokens:10,observedSince:now()-3600,lastTokenAt:now(),dailyUsage:[]},
  grok:{observedAt:now(),plan:'Synthetic fixture',remainingPercent:75,prepaidCents:1000,onDemandCapCents:null,onDemandUsedCents:null,periodEnd:new Date((now()+172800)*1000).toISOString()},openrouter:null
});
const ledgerDays=(seed,hidden)=>Array.from({length:21},(_,i)=>{const date=new Date(Date.UTC(2026,9,8+i)).toISOString().slice(0,10);const weighted=200000+((i*seed)%7)*60000;const explained=weighted*1e-5;const unattributed=i>=hidden?2+(i%3):0;return {date,points:explained+unattributed,weighted,explained,unattributed};});
const ledgerWindow=(id,label,seed,hidden,change)=>{const days=ledgerDays(seed,hidden);const points=days.reduce((s,d)=>s+d.points,0);const explained=days.reduce((s,d)=>s+d.explained,0);return {id,label,enough:true,activeDays:21,pointsPerMillion:10,pointsPerDollar:id.startsWith('codex')?null:0.8,points,explained,unattributed:points-explained,idlePoints:6,idleSteps:[now()-3600*3,now()-3600*27,now()-3600*51,now()-3600*75],change,days};};
const consistency={
  codex:{account:'fixture',days:28,limitHits:0,windows:[ledgerWindow('codex/primary','Codex · 5-hour',3,40,null),ledgerWindow('codex/secondary','Codex · weekly',5,14,{date:'2026-10-22',factor:1.8})],
    surfaces:[{surface:'codex',querySource:'',model:'gpt-5.6-sol',requests:812,input:2100000,output:640000,cacheRead:41000000,cacheWrite:0,costUsd:0}],cacheReadShare:0.95,
    serverVsLocal:Array.from({length:10},(_,i)=>({date:new Date(Date.UTC(2026,9,19+i)).toISOString().slice(0,10),server:9000000+i*400000,local:5200000+i*100000}))},
  claude:{account:'fixture',days:28,limitHits:2,windows:[ledgerWindow('five_hour','5-hour allowance',2,18,null),ledgerWindow('seven_day','7-day allowance',4,10,null)],
    surfaces:[{surface:'cli',querySource:'repl_main_thread',model:'claude-opus-5-5',requests:1420,input:380000,output:910000,cacheRead:96000000,cacheWrite:4100000,costUsd:212.4},
      {surface:'cli',querySource:'subagent',model:'claude-haiku-5-5',requests:510,input:120000,output:260000,cacheRead:1200000,cacheWrite:2900000,costUsd:9.1},
      {surface:'cloud',querySource:'repl_main_thread',model:'claude-opus-5-5',requests:96,input:40000,output:88000,cacheRead:8100000,cacheWrite:600000,costUsd:21.7}],
    cacheReadShare:0.91,serverVsLocal:[]}};
const alerts={codexFiveHour:{alert:'critical',alertReason:'pace',runsOutAt:now()+2400,surgeRatio:null},codexWeekly:{alert:'surge',alertReason:'surge',runsOutAt:now()+117000,surgeRatio:2.4},claudeFiveHour:{alert:'warning',alertReason:'pace',runsOutAt:now()+3600,surgeRatio:null}};
function compactRows(){
  const choices={codexFiveHour:['codex','Codex','5h',20],codexWeekly:['codex','Codex','wk',65],claudeFiveHour:['claude','Claude','5h',45],claudeWeekly:['claude','Claude','wk',null],grokAllowance:['grok','Grok','wk',75]};
  return (settings.compactWindows||['codexFiveHour','claudeFiveHour','grokAllowance','codexWeekly']).filter(id=>settings.providers[choices[id][0]]).slice(0,4).map(id=>({label:choices[id][1],period:choices[id][2],remainingPercent:choices[id][3],resetsAt:now()+7200,observedAt:now(),status:choices[id][3]===null?'unavailable':'fresh',...alerts[id]}));
}
window.previewUpdate=()=>{ for(const [event, callback] of listeners) callbacks.get(callback)?.({event,id:1,payload:event==='compact-updated'?compactRows():state()}); };
window.__TAURI_INTERNALS__={
  metadata:{currentWindow:{label:'main'},currentWebview:{label:'main'}},
  transformCallback(callback){const id=nextCallback++;callbacks.set(id,callback);return id;},
  unregisterCallback(id){callbacks.delete(id);},
  async invoke(command,args={}){
    if(command==='plugin:event|listen'){listeners.set(args.event,args.handler);return 1;}
    if(command==='plugin:event|unlisten')return;
    if(command==='plugin:app|version')return '0.4.0-preview';
    if(command==='get_dashboard'||command==='refresh_dashboard')return structuredClone(state());
    if(command==='get_compact_dashboard')return compactRows();
    if(command==='usage_relay_status'||command==='sync_relay')return null;
    if(command==='save_settings'){settings=structuredClone(args.settings);return structuredClone(state());}
    throw new Error('Preview does not perform '+command);
  }
};
