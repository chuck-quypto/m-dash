use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::ops::RangeInclusive;
use std::path::Path;

const DAY: u64 = 86_400;
/// Ranges offered on Allowance vs tokens. Per-request rows are kept 90 days.
pub const LEDGER_DAYS: [u64; 3] = [7, 28, 90];

pub struct Store(Connection);

/// Where a usage row ran, as the ledger and usage list name it: this computer's surface, "cloud",
/// or "other:<surface>" with "@<name>" once the sending computer has announced its name.
const WHERE: &str = "CASE WHEN source='relay' AND surface!='cloud' THEN 'other:'||surface||COALESCE('@'||(SELECT name FROM devices WHERE devices.origin=usage_events.origin),'') ELSE surface END";

/// Filters for the usage list. Empty fields match everything.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct UsageFilter {
    pub days: u64,
    /// One UTC day, YYYY-MM-DD, instead of the last `days`.
    pub date: String,
    #[serde(rename = "where")]
    pub place: String,
    pub model: String,
    pub purpose: String,
    pub page: u64,
}

const PAGE: u64 = 100;

/// Records the account's plan when it differs from the last one recorded, so the ledger can
/// date plan changes. Missing or empty plans record nothing.
fn record_plan(db: &Connection, account: &str, at: i64, plan: Option<&str>) -> Result<(), String> {
    let Some(plan) = plan.filter(|plan| !plan.is_empty()) else {
        return Ok(());
    };
    let error = |_| "Cannot save plan history.".to_string();
    let last: Option<String> = db
        .query_row(
            "SELECT text FROM ledger_marks WHERE account=?1 AND kind='plan' AND source='auto' AND at<=?2 ORDER BY at DESC LIMIT 1",
            params![account, at],
            |row| row.get(0),
        )
        .optional()
        .map_err(error)?;
    if last.as_deref() != Some(plan) {
        db.execute(
            "INSERT OR IGNORE INTO ledger_marks VALUES (?1,?2,'plan','auto',?3)",
            params![account, at, plan],
        )
        .map_err(error)?;
    }
    Ok(())
}

fn initialize_usage_events(db: &mut Connection) -> rusqlite::Result<()> {
    let transaction = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let (legacy, has_origin): (bool, bool) = transaction.query_row(
        "SELECT COUNT(*)>0 AND COUNT(*) FILTER (WHERE name='upload_seq')=0,
            COUNT(*) FILTER (WHERE name='origin')>0 FROM pragma_table_info('usage_events')",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if legacy {
        transaction.execute_batch("ALTER TABLE usage_events RENAME TO usage_events_legacy;")?;
    }
    transaction.execute_batch(
        "CREATE TABLE IF NOT EXISTS usage_events (upload_seq INTEGER PRIMARY KEY AUTOINCREMENT,
            account TEXT NOT NULL, source TEXT NOT NULL, key TEXT NOT NULL, occurred_at INTEGER NOT NULL,
            surface TEXT NOT NULL, model TEXT, effort TEXT, speed TEXT, query_source TEXT, agent TEXT, input INTEGER NOT NULL, output INTEGER NOT NULL,
            cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL, reasoning INTEGER, cost_micros INTEGER, origin TEXT, UNIQUE(account,key));",
    )?;
    if legacy {
        let origin = if has_origin { "origin" } else { "NULL" };
        // Keep existing cursor positions and start above any acknowledged row already pruned.
        transaction.execute_batch(&format!(
            "INSERT INTO usage_events (upload_seq,account,source,key,occurred_at,surface,model,effort,speed,query_source,agent,input,output,cache_read,cache_write,reasoning,cost_micros,origin)
                SELECT rowid,account,source,key,occurred_at,surface,model,effort,speed,query_source,agent,input,output,cache_read,cache_write,reasoning,cost_micros,{origin}
                FROM usage_events_legacy;
            DROP TABLE usage_events_legacy;
            INSERT INTO sqlite_sequence(name,seq) SELECT 'usage_events',0
                WHERE NOT EXISTS (SELECT 1 FROM sqlite_sequence WHERE name='usage_events');
            UPDATE sqlite_sequence SET seq=MAX(seq,COALESCE((
                SELECT MAX(pushed) FROM (
                    SELECT CASE WHEN json_valid(value) THEN json_extract(value,'$.pushed') END AS pushed
                    FROM settings WHERE key GLOB 'usage-relay:*'
                ) WHERE typeof(pushed)='integer'
            ),0)) WHERE name='usage_events';"
        ))?;
    }
    transaction.execute_batch(
        "CREATE INDEX IF NOT EXISTS usage_events_time ON usage_events(account,occurred_at);",
    )?;
    transaction.commit()
}

impl Store {
    pub fn claude_ping(&self) -> Result<crate::claude_ping::State, String> {
        let value: Option<String> = self
            .0
            .query_row(
                "SELECT value FROM settings WHERE key='claude-ping'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| "Cannot read Claude ping settings.")?;
        value
            .map(|text| {
                serde_json::from_str(&text).map_err(|_| {
                    "Claude ping settings are invalid. Automatic pings are unavailable.".into()
                })
            })
            .unwrap_or_else(|| Ok(crate::claude_ping::State::default()))
    }

    pub fn save_claude_ping(&self, state: &crate::claude_ping::State) -> Result<(), String> {
        let value = serde_json::to_string(state).map_err(|_| "Cannot encode Claude ping state.")?;
        self.0.execute("INSERT INTO settings (key,value) VALUES ('claude-ping',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [value])
            .map_err(|_| "Could not save Claude ping state. No new ping will be sent.")?;
        Ok(())
    }

    pub fn settings(&self) -> Result<Option<crate::settings::Settings>, String> {
        let text: Option<String> = self
            .0
            .query_row(
                "SELECT value FROM settings WHERE key='preferences'",
                [],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| "Cannot read saved settings.")?;
        text.map(|text| {
            let settings: crate::settings::Settings = serde_json::from_str(&text)
                .map_err(|_| "Saved settings could not be read. Your history was retained.")?;
            settings.validate()?;
            Ok(settings)
        })
        .transpose()
    }

    pub fn save_settings(&self, settings: &crate::settings::Settings) -> Result<(), String> {
        settings.validate()?;
        let text = serde_json::to_string(settings).map_err(|_| "Cannot encode settings.")?;
        self.0.execute("INSERT INTO settings (key,value) VALUES ('preferences',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [text])
            .map_err(|_| "Cannot save settings. Your previous choices were retained.")?;
        Ok(())
    }

    pub fn ingest_claude_usage(
        &mut self,
        snapshot: Value,
        now: u64,
    ) -> Result<Option<Value>, String> {
        let account = snapshot["accountId"]
            .as_str()
            .ok_or("Missing Claude account identity.")?;
        let at = snapshot["quotaObservedAt"]
            .as_u64()
            .filter(|at| *at <= now)
            .ok_or("Missing Claude quota observation time.")?;
        if !snapshot["windows"].is_array() {
            return Err("Missing Claude quota windows.".into());
        }
        let transaction = self
            .0
            .transaction()
            .map_err(|_| "Cannot begin saving Claude account usage.")?;
        transaction.execute("INSERT INTO claude_account_usage (account,quota,quota_at,updated_at,plan) VALUES (?1,?2,?3,?4,?5)
            ON CONFLICT(account) DO UPDATE SET quota=excluded.quota,quota_at=excluded.quota_at,updated_at=excluded.updated_at,plan=COALESCE(excluded.plan,claude_account_usage.plan)
            WHERE excluded.quota_at>=claude_account_usage.quota_at",
            params![account,snapshot["windows"].to_string(),at as i64,now as i64,snapshot["plan"].as_str()]).map_err(|_|"Cannot save Claude account usage.")?;
        record_plan(&transaction, account, at as i64, snapshot["plan"].as_str())?;
        transaction.execute("INSERT INTO settings (key,value) VALUES ('claude-current-account',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[account]).map_err(|_|"Cannot save Claude account selection.")?;
        transaction
            .commit()
            .map_err(|_| "Cannot finish saving Claude account usage.")?;
        self.save_claude_snapshot()
    }
    pub fn forecasts(&self, snapshot: &Value, now: u64) -> Result<Vec<Value>, String> {
        let account = snapshot["accountId"]
            .as_str()
            .ok_or("Missing forecast account.")?;
        let latest = snapshot["observedAt"].as_u64().unwrap_or(0);
        let mut query = self.0.prepare("SELECT observed_at,resources FROM observations WHERE account=?1 AND observed_at>=?2 AND observed_at<=?3 ORDER BY observed_at").map_err(|_|"Cannot read forecast history.")?;
        let rows = query
            .query_map(
                params![account, latest.saturating_sub(21600) as i64, latest as i64],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
            )
            .map_err(|_| "Cannot read forecast history.")?;
        let mut observations = Vec::new();
        for row in rows {
            let (at, text) = row.map_err(|_| "Cannot read forecast history.")?;
            let mut value: Value =
                serde_json::from_str(&text).map_err(|_| "Saved forecast history is invalid.")?;
            value["observedAt"] = json!(at);
            observations.push(value);
        }
        Ok(crate::forecast::estimate(snapshot, &observations, now))
    }
    pub fn attach_history(&self, snapshot: &mut Value) -> Result<(), String> {
        let account = snapshot["accountId"]
            .as_str()
            .ok_or("Missing history account.")?;
        let mut query = self
            .0
            .prepare("SELECT day,tokens FROM daily_tokens WHERE account=?1 ORDER BY day")
            .map_err(|_| "Cannot read retained token history.")?;
        let days: Vec<Value> = query
            .query_map([account], |row| {
                Ok(json!({"date":row.get::<_,String>(0)?,"tokens":row.get::<_,i64>(1)?}))
            })
            .map_err(|_| "Cannot read retained token history.")?
            .collect::<Result<_, _>>()
            .map_err(|_| "Cannot read retained token history.")?;
        if snapshot["provider"] == "openrouter" {
            snapshot["importedTokens"] = json!(days.iter().try_fold(0_i64, |sum, day| sum
                .checked_add(day["tokens"].as_i64().unwrap_or(0))));
            snapshot["historySince"] = days.first().map_or(Value::Null, |day| day["date"].clone());
            if days.is_empty() {
                snapshot["importedTokens"] = Value::Null;
            }
        }
        snapshot["dailyUsage"] = json!(days);
        Ok(())
    }

    /// Populates `dailyUsage`, `observedTokens`, `observedSince` and `lastTokenAt` from
    /// `usage_events` and rolled-up `usage_daily` rows for an account (e.g. Antigravity).
    pub fn attach_usage_history(&self, snapshot: &mut Value) -> Result<(), String> {
        let account = snapshot["accountId"]
            .as_str()
            .ok_or("Missing usage history account.")?;
        let error = |_| "Cannot read local usage history.".to_string();
        let retained = "SELECT day, SUM(input + output + cache_read + cache_write) AS tokens FROM usage_daily WHERE account=?1 AND day NOT IN (SELECT date(occurred_at,'unixepoch') FROM usage_events WHERE account=?1) GROUP BY day";
        let (since, last): (Option<i64>, Option<i64>) = self
            .0
            .query_row(
                &format!(
                    "SELECT (SELECT MIN(start) FROM (SELECT MIN(occurred_at) AS start FROM usage_events WHERE account=?1 UNION ALL SELECT CAST(strftime('%s',day) AS INTEGER) FROM ({retained}))), (SELECT MAX(occurred_at) FROM usage_events WHERE account=?1)"
                ),
                [account],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(error)?;
        let mut query = self
            .0
            .prepare(&format!(
                "SELECT date(occurred_at,'unixepoch'), SUM(input + output + cache_read + cache_write) FROM usage_events WHERE account=?1 GROUP BY 1 UNION ALL {retained} ORDER BY 1"
            ))
            .map_err(error)?;
        let days: Vec<Value> = query
            .query_map([account], |row| {
                Ok(json!({"date":row.get::<_,String>(0)?,"tokens":row.get::<_,i64>(1)?}))
            })
            .map_err(error)?
            .collect::<Result<_, _>>()
            .map_err(error)?;
        let total = (!days.is_empty())
            .then(|| {
                days.iter()
                    .try_fold(0_i64, |sum, day| sum.checked_add(day["tokens"].as_i64()?))
            })
            .flatten();
        snapshot["dailyUsage"] = json!(days);
        snapshot["observedTokens"] = json!(total);
        snapshot["observedSince"] = json!(since);
        snapshot["lastTokenAt"] = json!(last);
        Ok(())
    }
    pub fn ingest_claude_statusline(
        &mut self,
        value: Value,
        now: u64,
    ) -> Result<Option<Value>, String> {
        let status = crate::claude::normalize_status(value, now)?;
        self.0.execute("INSERT INTO claude_sessions (session,quota,api_duration_ms,quota_at,updated_at) VALUES (?1,?2,?3,?4,?4)
            ON CONFLICT(session) DO UPDATE SET quota=CASE WHEN claude_sessions.api_duration_ms IS NULL OR claude_sessions.api_duration_ms!=excluded.api_duration_ms THEN excluded.quota ELSE claude_sessions.quota END,
            quota_at=CASE WHEN claude_sessions.api_duration_ms IS NULL OR claude_sessions.api_duration_ms!=excluded.api_duration_ms THEN excluded.quota_at ELSE claude_sessions.quota_at END,
            api_duration_ms=excluded.api_duration_ms",
            params![status.session,status.windows.to_string(),status.api_duration_ms,now as i64]).map_err(|_| "Cannot save Claude quota update.")?;
        self.save_claude_snapshot()
    }

    pub fn ingest_claude_telemetry(
        &mut self,
        value: Value,
        now: u64,
    ) -> Result<Option<Value>, String> {
        let events = crate::claude::normalize_telemetry(value, now)?;
        let transaction = self
            .0
            .transaction()
            .map_err(|_| "Cannot begin saving Claude telemetry.")?;
        for event in events {
            let prior: Option<String> = transaction
                .query_row(
                    "SELECT account FROM claude_sessions WHERE session=?1",
                    [&event.session],
                    |row| row.get::<_, Option<String>>(0),
                )
                .optional()
                .map_err(|_| "Cannot check Claude session identity.")?
                .flatten();
            if prior
                .as_ref()
                .is_some_and(|account| account != &event.account)
            {
                return Err("Claude changed accounts within one session. Start a new session before collecting more data.".into());
            }
            transaction.execute("INSERT INTO claude_sessions (session,account,updated_at) VALUES (?1,?2,?3)
                ON CONFLICT(session) DO UPDATE SET account=excluded.account,updated_at=MAX(claude_sessions.updated_at,excluded.updated_at)",
                params![event.session,event.account,event.occurred_at]).map_err(|_| "Cannot save Claude session identity.")?;
            if let (Some(request), Some(tokens)) = (&event.request, event.tokens) {
                insert_claude_tokens(
                    &transaction,
                    &event.account,
                    request,
                    event.occurred_at,
                    tokens,
                )?;
                if let Some(detail) = &event.detail {
                    insert_usage(
                        &transaction,
                        &event.account,
                        "otel",
                        &format!("claude:{request}"),
                        event.occurred_at,
                        detail,
                    )?;
                }
            }
            if event.rate_limited {
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO limit_events VALUES (?1,?2,'rate_limited')",
                        params![event.account, event.occurred_at],
                    )
                    .map_err(|_| "Cannot save Claude limit event.")?;
            }
        }
        transaction
            .commit()
            .map_err(|_| "Cannot finish saving Claude telemetry.")?;
        self.save_claude_snapshot()
    }

    fn save_claude_snapshot(&mut self) -> Result<Option<Value>, String> {
        let snapshot = self.claude_snapshot()?;
        if let Some(value) = &snapshot {
            self.save(value)?;
        }
        Ok(snapshot)
    }

    pub fn claude_snapshot(&self) -> Result<Option<Value>, String> {
        let account: Option<String> = self.0.query_row("SELECT account FROM (
            SELECT account,updated_at,0 AS priority FROM claude_sessions WHERE account IS NOT NULL
            UNION ALL SELECT account,updated_at,1 FROM claude_account_usage
            UNION ALL SELECT value,0,2 FROM settings WHERE key='claude-current-account'
            UNION ALL SELECT account,observed_at,-1 FROM latest WHERE json_extract(snapshot,'$.provider')='claude'
            ) ORDER BY priority DESC,updated_at DESC LIMIT 1",[],
            |row| row.get(0)).optional().map_err(|_| "Cannot read Claude account identity.")?;
        let Some(account) = account else {
            return Ok(None);
        };
        let quota: Option<(String,i64,String,Option<String>)> = self.0.query_row("SELECT quota,quota_at,source,(SELECT plan FROM claude_account_usage WHERE account=?1) FROM (SELECT quota,quota_at,'Claude Code statusline' AS source FROM claude_sessions WHERE account=?1 AND quota IS NOT NULL UNION ALL SELECT quota,quota_at,'Claude Code account usage' AS source FROM claude_account_usage WHERE account=?1) ORDER BY quota_at DESC LIMIT 1",[&account],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional().map_err(|_| "Cannot read Claude quota history.")?;
        let (since,last): (Option<i64>,Option<i64>) = self.0.query_row("SELECT
            (SELECT MIN(start) FROM (SELECT MIN(occurred_at) AS start FROM claude_requests WHERE account=?1
                UNION ALL SELECT unixepoch(MIN(day)) FROM pruned_days WHERE ledger='tokens' AND account=?1)),
            (SELECT MAX(occurred_at) FROM claude_requests WHERE account=?1)",[&account],
            |row| Ok((row.get(0)?,row.get(1)?))).map_err(|_| "Cannot read Claude token totals.")?;
        let mut statement = self
            .0
            .prepare("SELECT day,tokens FROM daily_tokens WHERE account=?1 ORDER BY day")
            .map_err(|_| "Cannot read Claude daily history.")?;
        let days: Vec<Value> = statement
            .query_map([&account], |row| {
                Ok(json!({"date":row.get::<_,String>(0)?,"tokens":row.get::<_,i64>(1)?}))
            })
            .map_err(|_| "Cannot read Claude daily history.")?
            .collect::<Result<_, _>>()
            .map_err(|_| "Cannot read Claude daily history.")?;
        let tokens = (!days.is_empty())
            .then(|| {
                days.iter()
                    .try_fold(0_i64, |sum, day| sum.checked_add(day["tokens"].as_i64()?))
            })
            .flatten();
        let (windows, observed_at, source, plan) = match quota {
            Some((text, at, source, plan)) => (
                serde_json::from_str::<Value>(&text)
                    .map_err(|_| "Saved Claude quota is invalid.")?,
                Some(at),
                source,
                plan,
            ),
            None => (json!([]), None, "Claude Code local feed".into(), None),
        };
        Ok(Some(
            json!({"provider":"claude","accountId":account,"observedAt":observed_at.or(last).unwrap_or(0),
            "quotaObservedAt":observed_at,"windows":windows,"observedTokens":tokens,"observedSince":since,
            "lastTokenAt":last,"dailyUsage":days,"source":source,"plan":plan}),
        ))
    }
    /// Bounds long-running history: drops raw forecast readings and idle Claude sessions after
    /// 30 days, folds Claude request identifiers older than 35 whole UTC days into daily totals,
    /// folds per-request usage older than 90 whole UTC days into daily rollups, and keeps
    /// per-window allowance readings and limit events for 400 days.
    pub fn prune(&mut self, now: u64) -> Result<(), String> {
        let horizon = now.saturating_sub(30 * DAY) as i64;
        let request_cutoff = (now.saturating_sub(35 * DAY) / DAY * DAY) as i64;
        let usage_cutoff = (now.saturating_sub(90 * DAY) / DAY * DAY) as i64;
        let limit_cutoff = now.saturating_sub(400 * DAY) as i64;
        let transaction = self
            .0
            .transaction()
            .map_err(|_| "Cannot begin pruning usage history.")?;
        remember_pruned_keys(&transaction, request_cutoff, usage_cutoff)
            .map_err(|_| "Cannot retain pruned request accounting.")?;
        // SUM preserves unknown metrics and aborts the transaction on integer overflow.
        transaction.execute("WITH pending(account,day,surface,model,query_source,requests,input,output,cache_read,cache_write,reasoning,cost_micros) AS (
            SELECT account,date(occurred_at,'unixepoch'),surface,COALESCE(model,''),COALESCE(query_source,''),
                COUNT(*),SUM(input),SUM(output),SUM(cache_read),SUM(cache_write),SUM(reasoning),SUM(cost_micros)
            FROM usage_events e WHERE occurred_at<?1 AND NOT EXISTS (
                SELECT 1 FROM pruned_days WHERE ledger='usage' AND account=e.account AND day=date(e.occurred_at,'unixepoch') AND keys_known=0)
            GROUP BY 1,2,3,4,5)
            INSERT OR REPLACE INTO usage_daily SELECT account,day,surface,model,query_source,
                SUM(requests),SUM(input),SUM(output),SUM(cache_read),SUM(cache_write),SUM(reasoning),SUM(cost_micros)
            FROM (SELECT * FROM pending UNION ALL SELECT d.* FROM usage_daily d JOIN pending USING(account,day,surface,model,query_source))
            GROUP BY account,day,surface,model,query_source", [usage_cutoff])
            .map_err(|_| "Cannot retain daily usage rollups.")?;
        for (sql, value) in [
            (
                "DELETE FROM claude_requests WHERE occurred_at<?1",
                request_cutoff,
            ),
            (
                "DELETE FROM claude_sessions WHERE updated_at<?1 AND COALESCE(quota_at,0)<?1",
                horizon,
            ),
            ("DELETE FROM observations WHERE observed_at<?1", horizon),
            (
                "DELETE FROM usage_events WHERE occurred_at<?1",
                usage_cutoff,
            ),
            (
                "DELETE FROM limit_readings WHERE observed_at<?1",
                limit_cutoff,
            ),
            (
                "DELETE FROM limit_events WHERE occurred_at<?1",
                limit_cutoff,
            ),
        ] {
            transaction
                .execute(sql, [value])
                .map_err(|_| "Cannot prune usage history.")?;
        }
        transaction
            .commit()
            .map_err(|_| "Cannot finish pruning usage history.".into())
    }
    pub fn open(path: &Path) -> Result<Self, String> {
        let mut db = Connection::open(path).map_err(|_| "Cannot open local usage history.")?;
        db.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS latest (account TEXT PRIMARY KEY, observed_at INTEGER NOT NULL, snapshot TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS observations (account TEXT NOT NULL, observed_at INTEGER NOT NULL, resources TEXT NOT NULL, PRIMARY KEY(account, observed_at));
            CREATE TABLE IF NOT EXISTS daily_tokens (account TEXT NOT NULL, day TEXT NOT NULL, tokens INTEGER NOT NULL, PRIMARY KEY(account, day));
            CREATE TABLE IF NOT EXISTS claude_sessions (session TEXT PRIMARY KEY, account TEXT, quota TEXT, api_duration_ms INTEGER, quota_at INTEGER, updated_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS claude_requests (account TEXT NOT NULL, request TEXT NOT NULL, occurred_at INTEGER NOT NULL, tokens INTEGER NOT NULL, PRIMARY KEY(account,request));
            CREATE TABLE IF NOT EXISTS claude_account_usage (account TEXT PRIMARY KEY, quota TEXT NOT NULL, quota_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, plan TEXT);
            CREATE INDEX IF NOT EXISTS claude_requests_time ON claude_requests(account,occurred_at);
            CREATE TABLE IF NOT EXISTS usage_daily (account TEXT NOT NULL, day TEXT NOT NULL, surface TEXT NOT NULL, model TEXT NOT NULL, query_source TEXT NOT NULL,
                requests INTEGER NOT NULL, input INTEGER NOT NULL, output INTEGER NOT NULL, cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL,
                reasoning INTEGER, cost_micros INTEGER, PRIMARY KEY(account,day,surface,model,query_source));
            CREATE TABLE IF NOT EXISTS limit_readings (account TEXT NOT NULL, window TEXT NOT NULL, observed_at INTEGER NOT NULL, used_percent REAL NOT NULL,
                resets_at INTEGER, PRIMARY KEY(account,window,observed_at));
            CREATE TABLE IF NOT EXISTS limit_events (account TEXT NOT NULL, occurred_at INTEGER NOT NULL, kind TEXT NOT NULL, PRIMARY KEY(account,occurred_at,kind));
            CREATE TABLE IF NOT EXISTS log_cursors (provider TEXT NOT NULL, path TEXT NOT NULL, offset INTEGER NOT NULL, state TEXT NOT NULL, PRIMARY KEY(provider,path));
            CREATE TABLE IF NOT EXISTS devices (origin TEXT PRIMARY KEY, name TEXT NOT NULL, details TEXT NOT NULL, announced_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS ledger_marks (account TEXT NOT NULL, at INTEGER NOT NULL, kind TEXT NOT NULL, source TEXT NOT NULL, text TEXT NOT NULL,
                PRIMARY KEY(account,at,kind,source));")
            .map_err(|_| "Cannot initialize local usage history.")?;
        initialize_usage_events(&mut db).map_err(|_| "Cannot initialize local usage history.")?;
        migrate_pruned_days(&mut db).map_err(|_| "Cannot initialize retained daily usage.")?;
        // Plans seen before plan changes were recorded: the last 30 days of Codex observations.
        db.execute("INSERT OR IGNORE INTO ledger_marks SELECT account,observed_at,'plan','auto',plan FROM
            (SELECT account,observed_at,plan,LAG(plan) OVER (PARTITION BY account ORDER BY observed_at) AS previous FROM
                (SELECT account,observed_at,json_extract(resources,'$.plan') AS plan FROM observations WHERE json_valid(resources)
                    AND json_extract(resources,'$.plan')!='')) AS seen
            WHERE (previous IS NULL OR previous!=plan)
                AND NOT EXISTS (SELECT 1 FROM ledger_marks WHERE ledger_marks.account=seen.account AND kind='plan' AND source='auto')", [])
            .map_err(|_| "Cannot initialize plan history.")?;
        Ok(Self(db))
    }
    pub fn save(&mut self, snapshot: &Value) -> Result<(), String> {
        let account = snapshot["accountId"]
            .as_str()
            .ok_or("Snapshot is missing its account.")?;
        let time = snapshot["observedAt"]
            .as_i64()
            .filter(|value| *value >= 0)
            .ok_or("Snapshot is missing its observation time.")?;
        let resources = json!({"windows":snapshot["windows"],"credits":snapshot["credits"],"availableResets":snapshot["availableResets"],
            "prepaidCents":snapshot["prepaidCents"],"remainingPercent":snapshot["remainingPercent"],"periodEnd":snapshot["periodEnd"],
            "plan":snapshot["plan"],"totalCreditsUsd":snapshot["totalCreditsUsd"],"totalUsageUsd":snapshot["totalUsageUsd"],"balanceUsd":snapshot["balanceUsd"]});
        let transaction = self
            .0
            .transaction()
            .map_err(|_| "Cannot begin saving usage history.")?;
        transaction.execute("INSERT INTO latest VALUES (?1,?2,?3) ON CONFLICT(account) DO UPDATE SET observed_at=excluded.observed_at,snapshot=excluded.snapshot",
            params![account, time, snapshot.to_string()]).map_err(|_| "Cannot save latest account snapshot.")?;
        transaction
            .execute(
                "INSERT OR IGNORE INTO observations VALUES (?1,?2,?3)",
                params![account, time, resources.to_string()],
            )
            .map_err(|_| "Cannot save account observation.")?;
        record_plan(&transaction, account, time, snapshot["plan"].as_str())?;
        for (window, used, resets_at) in crate::usage::windows(snapshot) {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO limit_readings VALUES (?1,?2,?3,?4,?5)",
                    params![account, window, time, used, resets_at],
                )
                .map_err(|_| "Cannot save allowance reading.")?;
        }
        if let Some(days) = snapshot["dailyUsage"].as_array() {
            let claude = snapshot["provider"] == "claude";
            for day in days {
                let date = day["date"]
                    .as_str()
                    .ok_or("Daily usage is missing its date.")?;
                let tokens = day["tokens"]
                    .as_i64()
                    .filter(|value| *value >= 0)
                    .ok_or("Daily usage is missing its token total.")?;
                let written = transaction
                    .execute(
                        "INSERT INTO daily_tokens VALUES (?1,?2,?3)
                         ON CONFLICT(account,day) DO UPDATE SET tokens=excluded.tokens WHERE NOT ?4
                         AND NOT EXISTS (SELECT 1 FROM pruned_days WHERE ledger='tokens' AND account=?1 AND day=?2)",
                        params![account, date, tokens, claude],
                    )
                    .map_err(|_| "Cannot save daily token history.")?;
                if claude && written == 1 {
                    // A snapshot-only day came without request keys. Preserve its total on replay.
                    transaction
                        .execute(
                            "INSERT OR IGNORE INTO pruned_days VALUES ('tokens',?1,?2,0)",
                            params![account, date],
                        )
                        .map_err(|_| "Cannot retain Claude snapshot history.")?;
                }
            }
        }
        transaction
            .commit()
            .map_err(|_| "Cannot finish saving usage history.".into())
    }
    /// Saved read positions for one provider's local session logs.
    pub fn log_cursors(
        &self,
        provider: &str,
    ) -> Result<std::collections::HashMap<String, crate::local_logs::Cursor>, String> {
        let error = |_| "Cannot read local log positions.".to_string();
        let mut query = self
            .0
            .prepare("SELECT path,offset,state FROM log_cursors WHERE provider=?1")
            .map_err(error)?;
        let rows = query
            .query_map([provider], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        let mut cursors = std::collections::HashMap::new();
        for (path, saved_offset, text) in rows {
            let offset = saved_offset.max(0) as u64;
            let mut state = serde_json::from_str(&text).unwrap_or(Value::Null);
            if provider == "codex_rollout" {
                state = crate::local_logs::codex_cursor_state(&state, offset);
                let clean = state.to_string();
                if clean != text {
                    self.0.execute("UPDATE log_cursors SET state=?1 WHERE provider=?2 AND path=?3 AND offset=?4 AND state=?5",
                        params![clean, provider, path, saved_offset, text]).map_err(|_| "Cannot normalize local log positions.")?;
                }
            }
            cursors.insert(path, crate::local_logs::Cursor { offset, state });
        }
        Ok(cursors)
    }

    /// Saves one local-log pass for an account atomically with its new read positions.
    pub fn ingest_local_logs(
        &mut self,
        provider: &str,
        account: &str,
        scan: crate::local_logs::Scan,
    ) -> Result<(), String> {
        let transaction = self
            .0
            .transaction()
            .map_err(|_| "Cannot begin saving local session usage.")?;
        for (key, at, request) in &scan.requests {
            insert_usage(&transaction, account, provider, key, *at, request)?;
        }
        for (window, at, used, reset) in &scan.readings {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO limit_readings VALUES (?1,?2,?3,?4,?5)",
                    params![account, window, at, used, reset],
                )
                .map_err(|_| "Cannot save allowance reading.")?;
        }
        for (path, cursor) in &scan.cursors {
            let state = if provider == "codex_rollout" {
                crate::local_logs::codex_cursor_state(&cursor.state, cursor.offset).to_string()
            } else {
                cursor.state.to_string()
            };
            transaction.execute("INSERT INTO log_cursors VALUES (?1,?2,?3,?4) ON CONFLICT(provider,path) DO UPDATE SET offset=excluded.offset,state=excluded.state",
                params![provider, path, cursor.offset as i64, state]).map_err(|_| "Cannot save local log positions.")?;
        }
        transaction
            .commit()
            .map_err(|_| "Cannot finish saving local session usage.".into())
    }

    /// The burn ledger for one account over the last `days`, for the windows it reports now.
    pub fn consistency(
        &self,
        provider: crate::burn::Provider,
        snapshot: &Value,
        now: u64,
        days: u64,
    ) -> Result<Value, String> {
        self.ledger(provider, snapshot, now, days, None)
    }

    pub fn away_usage(
        &self,
        provider: crate::burn::Provider,
        snapshot: &Value,
        now: u64,
        span: RangeInclusive<i64>,
    ) -> Result<Value, String> {
        let days = LEDGER_DAYS[2];
        if span.start() >= span.end()
            || *span.start() < now.saturating_sub(days * DAY) as i64
            || *span.end() > now as i64
        {
            return Err(
                "Choose a start and end within the last 90 days, ending at or before now.".into(),
            );
        }
        self.ledger(provider, snapshot, now, days, Some(span))
    }

    fn ledger(
        &self,
        provider: crate::burn::Provider,
        snapshot: &Value,
        now: u64,
        days: u64,
        away: Option<RangeInclusive<i64>>,
    ) -> Result<Value, String> {
        let account = snapshot["accountId"]
            .as_str()
            .ok_or("Missing ledger account.")?;
        let since = now.saturating_sub(days * DAY) as i64;
        let until = away.as_ref().map_or(i64::MAX, |span| *span.end());
        let mut labels = std::collections::BTreeMap::new();
        for window in snapshot["windows"].as_array().into_iter().flatten() {
            let (Some(id), Some(label)) = (window["id"].as_str(), window["label"].as_str()) else {
                continue;
            };
            let period = match window["durationMins"].as_u64() {
                Some(300) => " · 5-hour",
                Some(10080) => " · weekly",
                _ => "",
            };
            let label = if label.contains("allowance") {
                label.to_owned()
            } else {
                format!("{label}{period}")
            };
            labels.insert(id.to_owned(), label);
        }
        let error = |_| "Cannot read the usage ledger.".to_string();
        let mut query = self.0.prepare("SELECT window,observed_at,used_percent,resets_at FROM limit_readings WHERE account=?1 AND observed_at BETWEEN ?2 AND ?3 ORDER BY observed_at").map_err(error)?;
        let readings = query
            .query_map(params![account, since, until], |row| {
                Ok(crate::burn::Reading {
                    window: row.get(0)?,
                    at: row.get(1)?,
                    used: row.get(2)?,
                    resets_at: row.get(3)?,
                })
            })
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?
            .into_iter()
            .filter(|reading| away.is_some() || labels.contains_key(&reading.window))
            .collect::<Vec<_>>();
        let mut query = self.0.prepare(&format!("SELECT occurred_at,{WHERE},COALESCE(model,''),COALESCE(query_source,''),input,output,cache_read,cache_write,cost_micros FROM usage_events WHERE account=?1 AND occurred_at BETWEEN ?2 AND ?3 ORDER BY occurred_at")).map_err(error)?;
        let usage = query
            .query_map(params![account, since, until], |row| {
                Ok(crate::burn::Usage {
                    at: row.get(0)?,
                    surface: row.get(1)?,
                    model: row.get(2)?,
                    query_source: row.get(3)?,
                    input: row.get(4)?,
                    output: row.get(5)?,
                    cache_read: row.get(6)?,
                    cache_write: row.get(7)?,
                    cost_micros: row.get(8)?,
                })
            })
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        let marks = self.ledger_marks(account, since)?;
        let breaks: Vec<i64> = marks
            .iter()
            .filter(|mark| mark["kind"] == "plan")
            .filter_map(|mark| mark["at"].as_i64())
            .collect();
        if let Some(span) = away {
            let mut result =
                crate::burn::away_usage(provider, &labels, &readings, &usage, &breaks, span);
            result["account"] = json!(account);
            return Ok(result);
        }
        let mut server = std::collections::BTreeMap::new();
        if provider == crate::burn::Provider::Codex {
            let mut query = self
                .0
                .prepare("SELECT day,tokens FROM daily_tokens WHERE account=?1 AND day>=date(?2,'unixepoch')")
                .map_err(error)?;
            for row in query
                .query_map(params![account, since], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
                })
                .map_err(error)?
            {
                let (day, tokens) = row.map_err(error)?;
                server.insert(day, tokens);
            }
        }
        let limit_hits: i64 = self
            .0
            .query_row(
                "SELECT COUNT(*) FROM limit_events WHERE account=?1 AND occurred_at>=?2",
                params![account, since],
                |row| row.get(0),
            )
            .map_err(error)?;
        let mut result =
            crate::burn::analyze(provider, &labels, &readings, &usage, &server, &breaks);
        result["marks"] = json!(marks);
        result["account"] = json!(account);
        result["days"] = json!(days);
        result["limitHits"] = json!(limit_hits);
        Ok(result)
    }

    /// Plan changes and the owner's notes since `since`. Recorded plans become changes only after
    /// the first one, which is the plan m-dash first saw rather than a change.
    fn ledger_marks(&self, account: &str, since: i64) -> Result<Vec<Value>, String> {
        let error = |_| "Cannot read plan changes and notes.".to_string();
        let mut query = self
            .0
            .prepare("SELECT at,kind,source,text FROM ledger_marks WHERE account=?1 ORDER BY at")
            .map_err(error)?;
        let rows = query
            .query_map([account], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                ))
            })
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        let mut previous: Option<String> = None;
        let mut marks = Vec::new();
        for (at, kind, source, text) in rows {
            let shown = if source == "auto" {
                let change = previous
                    .replace(text.clone())
                    .map(|from| format!("Plan changed from {from} to {text}"));
                let Some(change) = change else { continue };
                change
            } else {
                text
            };
            if at >= since {
                marks.push(json!({"at":at,"kind":kind,"source":source,"text":shown}));
            }
        }
        Ok(marks)
    }

    /// Adds the owner's note, or a plan change m-dash could not see, on a day of the ledger.
    pub fn add_ledger_mark(
        &self,
        account: &str,
        at: i64,
        plan: bool,
        text: &str,
    ) -> Result<(), String> {
        let text = text.trim();
        if text.is_empty() || text.chars().count() > 200 {
            return Err("Write a note of up to 200 characters.".into());
        }
        self.0
            .execute(
                "INSERT INTO ledger_marks VALUES (?1,?2,?3,'manual',?4) ON CONFLICT DO UPDATE SET text=excluded.text",
                params![account, at, if plan { "plan" } else { "note" }, text],
            )
            .map_err(|_| "Cannot save the note.")?;
        Ok(())
    }

    pub fn remove_ledger_mark(&self, account: &str, at: i64, kind: &str) -> Result<(), String> {
        self.0
            .execute(
                "DELETE FROM ledger_marks WHERE account=?1 AND at=?2 AND kind=?3 AND source='manual'",
                params![account, at, kind],
            )
            .map_err(|_| "Cannot remove the note.")?;
        Ok(())
    }

    pub fn setting(&self, key: &str) -> Result<Option<String>, String> {
        self.0
            .query_row("SELECT value FROM settings WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .optional()
            .map_err(|_| "Cannot read a saved setting.".into())
    }

    pub fn save_setting(&self, key: &str, value: &str) -> Result<(), String> {
        self.0
            .execute("INSERT INTO settings (key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value", params![key, value])
            .map_err(|_| "Cannot save a setting.")?;
        Ok(())
    }

    /// The computers and cloud sessions that report usage to this one: each other computer's
    /// name, setup and last usage, and when a cloud session last reported.
    pub fn computers(&self) -> Result<Value, String> {
        let error = |_| "Cannot read shared computers.".to_string();
        let mut query = self
            .0
            .prepare(
                "SELECT origin,name,details,announced_at,(SELECT MAX(occurred_at) FROM usage_events WHERE usage_events.origin=devices.origin) FROM devices
                UNION ALL SELECT origin,NULL,NULL,NULL,MAX(occurred_at) FROM usage_events WHERE source='relay' AND origin IS NOT NULL AND origin!='cloud'
                    AND origin NOT IN (SELECT origin FROM devices) GROUP BY origin",
            )
            .map_err(error)?;
        let others = query
            .query_map([], |row| {
                Ok(json!({"name":row.get::<_, Option<String>>(1)?,"details":row.get::<_, Option<String>>(2)?,
                    "announcedAt":row.get::<_, Option<i64>>(3)?,"lastUsage":row.get::<_, Option<i64>>(4)?}))
            })
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        let cloud: Option<i64> = self
            .0
            .query_row(
                "SELECT MAX(occurred_at) FROM usage_events WHERE source='relay' AND surface='cloud'",
                [],
                |row| row.get(0),
            )
            .map_err(error)?;
        Ok(json!({"others":others,"cloudLastUsage":cloud}))
    }

    /// One page of an account's usage rows, newest first, with the allowance steps in the same
    /// span, the filter choices available and, with `csv`, every matching row instead of a page.
    pub fn usage_list(
        &self,
        account: &str,
        filter: &UsageFilter,
        now: u64,
        csv: bool,
    ) -> Result<Value, String> {
        let error = |_| "Cannot read the usage list.".to_string();
        let (from, to) = if filter.date.is_empty() {
            let days = if LEDGER_DAYS.contains(&filter.days) {
                filter.days
            } else {
                28
            };
            (now.saturating_sub(days * DAY) as i64, i64::MAX)
        } else {
            let day: i64 = self
                .0
                .query_row("SELECT unixepoch(?1)", [&filter.date], |row| {
                    row.get::<_, Option<i64>>(0)
                })
                .map_err(error)?
                .ok_or("Choose a valid date.")?;
            (day, day + DAY as i64)
        };
        let matching = format!(
            "FROM usage_events WHERE account=?1 AND occurred_at>=?2 AND occurred_at<?3
            AND (?4='' OR {WHERE}=?4) AND (?5='' OR COALESCE(model,'')=?5) AND (?6='' OR COALESCE(query_source,'')=?6)"
        );
        let params = params![
            account,
            from,
            to,
            filter.place,
            filter.model,
            filter.purpose
        ];
        let total: i64 = self
            .0
            .query_row(&format!("SELECT COUNT(*) {matching}"), params, |row| {
                row.get(0)
            })
            .map_err(error)?;
        let (limit, offset) = if csv {
            (20_000, 0)
        } else {
            (PAGE as i64, (filter.page * PAGE) as i64)
        };
        let mut query = self
            .0
            .prepare(&format!(
                "SELECT occurred_at,{WHERE},COALESCE(model,''),COALESCE(query_source,''),input,output,cache_read,cache_write,cost_micros
                {matching} ORDER BY occurred_at DESC,upload_seq DESC LIMIT {limit} OFFSET {offset}"
            ))
            .map_err(error)?;
        let rows = query
            .query_map(params, |row| {
                Ok(json!({"at":row.get::<_, i64>(0)?,"where":row.get::<_, String>(1)?,"model":row.get::<_, String>(2)?,
                    "purpose":row.get::<_, String>(3)?,"input":row.get::<_, i64>(4)?,"output":row.get::<_, i64>(5)?,
                    "cacheRead":row.get::<_, i64>(6)?,"cacheWrite":row.get::<_, i64>(7)?,"costMicros":row.get::<_, Option<i64>>(8)?}))
            })
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        if csv {
            let mut text = String::from(
                "time_utc,where,model,purpose,input,output,cache_read,cache_write,api_price_usd\n",
            );
            let quote = |value: &Value| {
                format!(
                    "\"{}\"",
                    value.as_str().unwrap_or_default().replace('"', "\"\"")
                )
            };
            for row in &rows {
                let time = time::OffsetDateTime::from_unix_timestamp(
                    row["at"].as_i64().unwrap_or_default(),
                )
                .ok()
                .and_then(|time| {
                    time.format(&time::format_description::well_known::Rfc3339)
                        .ok()
                })
                .unwrap_or_default();
                let price = row["costMicros"]
                    .as_i64()
                    .map(|micros| format!("{:.6}", micros as f64 / 1e6))
                    .unwrap_or_default();
                text.push_str(&format!(
                    "{time},{},{},{},{},{},{},{},{price}\n",
                    quote(&row["where"]),
                    quote(&row["model"]),
                    quote(&row["purpose"]),
                    row["input"],
                    row["output"],
                    row["cacheRead"],
                    row["cacheWrite"]
                ));
            }
            return Ok(json!({"csv":text,"total":total,"truncated":total > rows.len() as i64}));
        }
        // Allowance steps in the page's span: each reading that moved up from the one before it.
        let span = (
            rows.last()
                .and_then(|row| row["at"].as_i64())
                .unwrap_or(from),
            rows.first()
                .and_then(|row| row["at"].as_i64())
                .unwrap_or(now as i64),
        );
        let mut query = self
            .0
            .prepare(
                "SELECT window,observed_at,used_percent,previous FROM (SELECT window,observed_at,used_percent,
                LAG(used_percent) OVER (PARTITION BY window ORDER BY observed_at) AS previous FROM limit_readings WHERE account=?1 AND observed_at>=?2-86400 AND observed_at<=?3)
                WHERE observed_at>=?2 AND previous IS NOT NULL AND used_percent>previous ORDER BY observed_at DESC LIMIT 500",
            )
            .map_err(error)?;
        let steps = query
            .query_map(params![account, span.0, span.1 + 600], |row| {
                Ok(json!({"window":row.get::<_, String>(0)?,"at":row.get::<_, i64>(1)?,"used":row.get::<_, f64>(2)?,"previous":row.get::<_, f64>(3)?}))
            })
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?;
        let choices = |column: &str| -> Result<Vec<String>, String> {
            let mut query = self
                .0
                .prepare(&format!(
                    "SELECT DISTINCT {column} AS value FROM usage_events WHERE account=?1 AND occurred_at>=?2 AND occurred_at<?3 ORDER BY value"
                ))
                .map_err(error)?;
            let values = query
                .query_map(params![account, from, to], |row| row.get::<_, String>(0))
                .map_err(error)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(error)?;
            Ok(values)
        };
        Ok(
            json!({"rows":rows,"steps":steps,"total":total,"page":filter.page,"pageSize":PAGE,
            "choices":{"where":choices(WHERE)?,"model":choices("COALESCE(model,'')")?,"purpose":choices("COALESCE(query_source,'')")?}}),
        )
    }

    /// The saved Allowance vs tokens range, 28 days unless 7 or 90 was chosen.
    pub fn ledger_days(&self) -> u64 {
        self.0
            .query_row(
                "SELECT value FROM settings WHERE key='ledger-days'",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
            .and_then(|text| text.parse().ok())
            .filter(|days| LEDGER_DAYS.contains(days))
            .unwrap_or(28)
    }

    pub fn save_ledger_days(&self, days: u64) -> Result<(), String> {
        if !LEDGER_DAYS.contains(&days) {
            return Err("Choose 7, 28 or 90 days.".into());
        }
        self.0.execute("INSERT INTO settings (key,value) VALUES ('ledger-days',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [days.to_string()])
            .map_err(|_| "Cannot save the Allowance vs tokens range.")?;
        Ok(())
    }

    fn relay_state(&self, channel: &str) -> Result<Value, String> {
        let text: Option<String> = self
            .0
            .query_row(
                "SELECT value FROM settings WHERE key=?1",
                [format!("usage-relay:{channel}")],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| "Cannot read usage channel progress.")?;
        Ok(text
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_else(|| json!({"pushed":0,"seq":0})))
    }

    fn save_relay_state(db: &Connection, channel: &str, state: &Value) -> Result<(), String> {
        db.execute("INSERT INTO settings (key,value) VALUES (?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            params![format!("usage-relay:{channel}"), state.to_string()]).map_err(|_| "Cannot save usage channel progress.")?;
        Ok(())
    }

    /// The next upload page and the `upload_seq` to mark after the relay accepts it.
    /// Received rows and rows older than the relay's 14-day retention are excluded.
    pub fn unshared_usage(
        &self,
        channel: &str,
        device: &str,
        limit: u16,
    ) -> Result<(i64, Vec<Value>), String> {
        let after = self.relay_state(channel)?["pushed"].as_i64().unwrap_or(0);
        let error = |_| "Cannot read usage for the usage channel.".to_string();
        let mut query = self.0.prepare("SELECT upload_seq,account,key,occurred_at,surface,model,effort,speed,query_source,agent,input,output,cache_read,cache_write,reasoning,cost_micros
            FROM usage_events WHERE upload_seq>?1 AND source!='relay' AND occurred_at>=CAST(strftime('%s','now') AS INTEGER)-1209600 ORDER BY upload_seq LIMIT ?2").map_err(error)?;
        let candidates = query
            .query_map(params![after, limit.min(200)], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    crate::usage::Request {
                        surface: row.get(4)?,
                        model: row.get(5)?,
                        effort: row.get(6)?,
                        speed: row.get(7)?,
                        query_source: row.get(8)?,
                        agent: row.get(9)?,
                        input: row.get(10)?,
                        output: row.get(11)?,
                        cache_read: row.get(12)?,
                        cache_write: row.get(13)?,
                        reasoning: row.get(14)?,
                        cost_micros: row.get(15)?,
                    },
                ))
            })
            .map_err(error)?;
        let mut rows = Vec::new();
        let mut bytes = br#"{"rows":[]}"#.len();
        let mut last = after;
        for candidate in candidates {
            let (sequence, account, key, at, request) = candidate.map_err(error)?;
            let row = crate::usage_relay::wire(&account, &key, at, &request, device);
            bytes += row.to_string().len() + usize::from(!rows.is_empty());
            if bytes > m_dash_sync::MAX_BODY {
                if rows.is_empty() {
                    return Err("One usage row exceeds the relay's upload size limit.".into());
                }
                break;
            }
            rows.push(row);
            last = last.max(sequence);
        }
        Ok((last, rows))
    }

    pub fn mark_shared(&mut self, channel: &str, through: i64) -> Result<(), String> {
        let mut state = self.relay_state(channel)?;
        state["pushed"] = json!(through.max(state["pushed"].as_i64().unwrap_or(0)));
        Self::save_relay_state(&self.0, channel, &state)
    }

    pub fn relay_cursor(&self, channel: &str) -> Result<i64, String> {
        Ok(self.relay_state(channel)?["seq"].as_i64().unwrap_or(0))
    }

    /// Commits the rows, sending computers and cursor of a downloaded page together.
    pub fn ingest_relay(
        &mut self,
        channel: &str,
        rows: &[(i64, crate::usage::Keyed)],
        origins: &[(String, String, String)],
        devices: &[crate::usage_relay::Announced],
        through: i64,
    ) -> Result<(), String> {
        let mut state = self.relay_state(channel)?;
        let transaction = self
            .0
            .transaction()
            .map_err(|_| "Cannot begin saving shared usage.")?;
        for (_, (account, key, at, request)) in rows {
            insert_usage(&transaction, account, "relay", key, *at, request)?;
        }
        let error = |_| "Cannot save shared computer details.".to_string();
        for (account, key, origin) in origins {
            transaction
                .execute(
                    "UPDATE usage_events SET origin=?3 WHERE account=?1 AND key=?2 AND source='relay'",
                    params![account, key, origin],
                )
                .map_err(error)?;
        }
        for device in devices {
            transaction
                .execute(
                    "INSERT INTO devices VALUES (?1,?2,?3,?4) ON CONFLICT(origin) DO UPDATE SET name=excluded.name,
                    details=excluded.details,announced_at=excluded.announced_at WHERE excluded.announced_at>=devices.announced_at",
                    params![device.origin, device.name, device.details, device.at],
                )
                .map_err(error)?;
        }
        state["seq"] = json!(through);
        Self::save_relay_state(&transaction, channel, &state)?;
        transaction
            .commit()
            .map_err(|_| "Cannot finish saving shared usage.".into())
    }

    pub fn latest(&self, provider: &str) -> Result<Option<Value>, String> {
        let row: Option<String> = self
            .0
            .query_row(
                "SELECT snapshot FROM latest WHERE COALESCE(json_extract(snapshot,'$.provider'),'codex')=?1 ORDER BY observed_at DESC LIMIT 1",
                [provider],
                |row| row.get(0),
            )
            .optional()
            .map_err(|_| "Cannot read saved account snapshot.")?;
        row.map(|text| {
            serde_json::from_str(&text).map_err(|_| "Saved account snapshot is invalid.".into())
        })
        .transpose()
    }
    pub fn observation_count(&self, account: &str) -> Result<i64, String> {
        self.0
            .query_row(
                "SELECT COUNT(*) FROM observations WHERE account=?1",
                [account],
                |row| row.get(0),
            )
            .map_err(|_| "Cannot read observation count.".into())
    }
}

fn migrate_pruned_days(db: &mut Connection) -> rusqlite::Result<()> {
    let tx = db.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let version: u32 = tx.pragma_query_value(None, "user_version", |r| r.get(0))?;
    if version == 0 {
        // Older archives have no keys to distinguish a late arrival from a replay.
        tx.execute_batch("CREATE TABLE pruned_days (ledger TEXT NOT NULL, account TEXT NOT NULL, day TEXT NOT NULL,
                keys_known INTEGER NOT NULL, PRIMARY KEY(ledger,account,day)) WITHOUT ROWID;
            CREATE TABLE pruned_keys (digest BLOB PRIMARY KEY) WITHOUT ROWID;
            INSERT INTO pruned_days SELECT DISTINCT 'usage',account,day,0 FROM usage_daily;
            WITH telemetry AS (SELECT account,date(occurred_at,'unixepoch') AS day,COUNT(*) AS requests
                FROM usage_events WHERE source='otel' GROUP BY 1,2),
            live AS (SELECT r.account,date(r.occurred_at,'unixepoch') AS day,SUM(r.tokens) AS tokens,
                    COUNT(*) AS requests,COUNT(e.key) AS matched
                FROM claude_requests r LEFT JOIN usage_events e
                    ON e.account=r.account AND e.key='claude:'||r.request AND e.source='otel'
                    AND date(e.occurred_at,'unixepoch')=date(r.occurred_at,'unixepoch')
                    AND e.input+e.output+e.cache_read+e.cache_write=r.tokens
                GROUP BY 1,2)
            INSERT INTO pruned_days SELECT 'tokens',d.account,d.day,0 FROM daily_tokens d LEFT JOIN live USING(account,day)
                LEFT JOIN telemetry USING(account,day)
                WHERE (d.account GLOB 'claude:*' OR live.tokens IS NOT NULL OR EXISTS (
                    SELECT 1 FROM latest WHERE account=d.account AND json_extract(snapshot,'$.provider')='claude'))
                AND (live.tokens IS NULL OR live.tokens<d.tokens OR live.matched!=live.requests
                    OR live.matched!=telemetry.requests OR EXISTS (
                    SELECT 1 FROM pruned_days WHERE ledger='usage' AND account=d.account AND day=d.day));
            INSERT INTO daily_tokens SELECT account,date(occurred_at,'unixepoch'),SUM(tokens) FROM claude_requests GROUP BY 1,2
                ON CONFLICT(account,day) DO UPDATE SET tokens=excluded.tokens WHERE NOT EXISTS (
                    SELECT 1 FROM pruned_days WHERE ledger='tokens' AND account=excluded.account AND day=excluded.day);
            PRAGMA user_version=1;")?;
    }
    tx.commit()
}

fn pruned_key(ledger: &str, account: &str, request: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    for part in [ledger, account, request] {
        hash.update((part.len() as u64).to_le_bytes());
        hash.update(part.as_bytes());
    }
    hash.finalize().into()
}

fn remember_pruned_keys(
    db: &rusqlite::Transaction<'_>,
    tokens_before: i64,
    usage_before: i64,
) -> rusqlite::Result<()> {
    let mut query = db.prepare("SELECT 'tokens',account,request,date(occurred_at,'unixepoch') FROM claude_requests WHERE occurred_at<?1
        UNION ALL SELECT 'usage',account,key,date(occurred_at,'unixepoch') FROM usage_events WHERE occurred_at<?2")?;
    let mut rows = query.query(params![tokens_before, usage_before])?;
    let mut key = db.prepare("INSERT OR IGNORE INTO pruned_keys VALUES (?1)")?;
    let mut day = db.prepare("INSERT OR IGNORE INTO pruned_days VALUES (?1,?2,?3,1)")?;
    while let Some(row) = rows.next()? {
        let ledger: String = row.get(0)?;
        let account: String = row.get(1)?;
        let request: String = row.get(2)?;
        let date: String = row.get(3)?;
        key.execute([pruned_key(&ledger, &account, &request)])?;
        day.execute(params![ledger, account, date])?;
    }
    Ok(())
}

fn insert_claude_tokens(
    db: &rusqlite::Transaction<'_>,
    account: &str,
    request: &str,
    at: i64,
    tokens: i64,
) -> Result<(), String> {
    let inserted = db
        .execute(
            "INSERT INTO claude_requests SELECT ?1,?2,?3,?4
             WHERE NOT EXISTS (SELECT 1 FROM pruned_keys WHERE digest=?5)
               AND NOT EXISTS (SELECT 1 FROM pruned_days WHERE ledger='tokens' AND account=?1 AND day=date(?3,'unixepoch') AND keys_known=0)
             ON CONFLICT(account,request) DO NOTHING",
            params![account, request, at, tokens, pruned_key("tokens", account, request)],
        )
        .map_err(|_| "Cannot save Claude token event.")?;
    if inserted == 1 {
        db.execute("INSERT INTO daily_tokens VALUES (?1,date(?2,'unixepoch'),?3)
            ON CONFLICT(account,day) DO UPDATE SET tokens=(
                SELECT SUM(amount) FROM (SELECT daily_tokens.tokens AS amount UNION ALL SELECT excluded.tokens))",
            params![account, at, tokens]).map_err(|_| "Cannot save Claude daily tokens.")?;
    }
    Ok(())
}

/// Records one request once per account and key; a replay is ignored.
/// Telemetry carries cost, effort and surface that a transcript lacks, so it replaces one.
pub fn insert_usage(
    db: &Connection,
    account: &str,
    source: &str,
    key: &str,
    occurred_at: i64,
    request: &crate::usage::Request,
) -> Result<(), String> {
    db.execute(
        "INSERT INTO usage_events (account,source,key,occurred_at,surface,model,effort,speed,query_source,agent,input,output,cache_read,cache_write,reasoning,cost_micros)
        SELECT ?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16
        WHERE NOT EXISTS (SELECT 1 FROM pruned_keys WHERE digest=?17)
          AND NOT EXISTS (SELECT 1 FROM pruned_days WHERE ledger='usage' AND account=?1 AND day=date(?4,'unixepoch') AND keys_known=0)
        ON CONFLICT(account,key) DO UPDATE SET source=excluded.source,occurred_at=excluded.occurred_at,surface=excluded.surface,
        model=excluded.model,effort=excluded.effort,speed=excluded.speed,query_source=excluded.query_source,agent=excluded.agent,
        input=excluded.input,output=excluded.output,cache_read=excluded.cache_read,cache_write=excluded.cache_write,
        reasoning=excluded.reasoning,cost_micros=excluded.cost_micros
        WHERE excluded.source='otel' AND usage_events.source!='otel'",
        params![
            account,
            source,
            key,
            occurred_at,
            request.surface,
            request.model,
            request.effort,
            request.speed,
            request.query_source,
            request.agent,
            request.input,
            request.output,
            request.cache_read,
            request.cache_write,
            request.reasoning,
            request.cost_micros,
            pruned_key("usage", account, key)
        ],
    )
    .map_err(|_| "Cannot save usage event.")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upgrading_usage_events_keeps_legacy_rows_and_their_later_origin() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.sqlite");
        Connection::open(&path).unwrap().execute_batch(
            "CREATE TABLE usage_events (account TEXT NOT NULL, source TEXT NOT NULL, key TEXT NOT NULL, occurred_at INTEGER NOT NULL,
                surface TEXT NOT NULL, model TEXT, effort TEXT, speed TEXT, query_source TEXT, agent TEXT, input INTEGER NOT NULL, output INTEGER NOT NULL,
                cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL, reasoning INTEGER, cost_micros INTEGER, PRIMARY KEY(account,key));
             INSERT INTO usage_events (account,source,key,occurred_at,surface,input,output,cache_read,cache_write,cost_micros)
                VALUES ('one','relay','request',100,'cli',17,3,0,0,29);",
        ).unwrap();
        let saved = |store: &Store| -> (i64, i64, i64, Option<String>) {
            store.0.query_row(
                "SELECT input,output,cost_micros,origin FROM usage_events WHERE account='one' AND key='request'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).unwrap()
        };
        let mut store = Store::open(&path).unwrap();
        assert_eq!(saved(&store), (17, 3, 29, None));
        store
            .ingest_relay(
                "chan",
                &[],
                &[("one".into(), "request".into(), "sender".into())],
                &[],
                0,
            )
            .unwrap();
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(saved(&reopened), (17, 3, 29, Some("sender".into())));
    }

    #[test]
    fn native_account_switches_are_authoritative_even_with_identical_timestamps() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite");
        let mut store = Store::open(&path).unwrap();
        for account in ["claude:one", "claude:two", "claude:one"] {
            store
                .ingest_claude_usage(
                    json!({"accountId":account,"quotaObservedAt":1000,"windows":[]}),
                    1000,
                )
                .unwrap();
            assert_eq!(
                store.claude_snapshot().unwrap().unwrap()["accountId"],
                account
            );
        }
        drop(store);
        assert_eq!(
            Store::open(&path)
                .unwrap()
                .claude_snapshot()
                .unwrap()
                .unwrap()["accountId"],
            "claude:one"
        );
    }
    #[test]
    fn feed_keeps_known_plan_and_cannot_switch_the_selected_native_account() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("usage.sqlite")).unwrap();
        let identity = |account: &str, session: &str| {
            json!({"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"claude-code"}}]},"scopeLogs":[{"logRecords":[{"attributes":[
            {"key":"user.account_uuid","value":{"stringValue":account}},{"key":"organization.id","value":{"stringValue":"org"}},{"key":"session.id","value":{"stringValue":session}}]}]}]}]})
        };
        store
            .ingest_claude_telemetry(identity("one", "s"), 1000)
            .unwrap();
        let mut quota = json!({"accountId":"claude:[\"one\",\"org\"]","quotaObservedAt":1000,"observedAt":1000,"windows":[{"id":"five_hour","label":"5-hour allowance","durationMins":300,"remainingPercent":90,"resetsAt":10000}],"plan":"pro"});
        store.ingest_claude_usage(quota.clone(), 1000).unwrap();
        quota["plan"] = Value::Null;
        store.ingest_claude_usage(quota, 1001).unwrap();
        for (at, used) in [(1600, 20), (2200, 30)] {
            store.ingest_claude_statusline(json!({"session_id":"s","cost":{"total_api_duration_ms":at},"rate_limits":{"five_hour":{"used_percentage":used,"resets_at":10000}}}),at).unwrap();
        }
        let snapshot = store.claude_snapshot().unwrap().unwrap();
        assert_eq!(snapshot["plan"], "pro");
        assert_eq!(
            store.forecasts(&snapshot, 2200).unwrap()[0]["perHour"],
            60.0
        );
        store
            .ingest_claude_telemetry(identity("other", "other"), 2300)
            .unwrap();
        assert_eq!(
            store.claude_snapshot().unwrap().unwrap()["accountId"],
            "claude:[\"one\",\"org\"]"
        );
    }
    use serde_json::json;

    #[test]
    fn provider_choices_survive_restart_without_removing_saved_history() {
        use crate::settings::{CompactWindow, Providers, Settings};
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite");
        let mut store = Store::open(&path).unwrap();
        assert_eq!(store.settings().unwrap(), None, "first launch needs setup");
        let snapshot = json!({"accountId":"fixture","observedAt":100,"windows":[],"dailyUsage":[{"date":"2026-10-01","tokens":123}]});
        store.save(&snapshot).unwrap();
        let selected = Settings {
            providers: Providers {
                claude: true,
                ..Default::default()
            },
            local_logs: false,
            compact_windows: Some(vec![CompactWindow::ClaudeFiveHour]),
        };
        store.save_settings(&selected).unwrap();
        drop(store);
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.settings().unwrap(), Some(selected));
        assert_eq!(reopened.latest("codex").unwrap(), Some(snapshot));
        assert_eq!(reopened.observation_count("fixture").unwrap(), 1);
    }

    #[test]
    fn ping_consent_and_claim_survive_restart_without_changing_preferences() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite");
        let store = Store::open(&path).unwrap();
        let preferences = crate::settings::Settings::default();
        store.save_settings(&preferences).unwrap();
        let mut ping = store.claude_ping().unwrap();
        assert_eq!(
            ping.status()["enabled"],
            false,
            "existing installations stay off"
        );
        ping.configure(Some("claude:one"), true).unwrap();
        let snapshot = json!({"accountId":"claude:one","source":"Claude Code account usage","plan":"pro","pingFresh":true,
            "quotaObservedAt":1000,"fiveHourState":"idle","windows":[{"id":"seven_day","remainingPercent":80,"resetsAt":90000}]});
        assert!(ping.observe(&snapshot, 1000).is_some());
        store.save_claude_ping(&ping).unwrap();
        drop(store);
        let store = Store::open(&path).unwrap();
        let mut ping = store.claude_ping().unwrap();
        assert!(ping.observe(&snapshot, 1000).is_none());
        assert_eq!(store.settings().unwrap(), Some(preferences));
    }

    #[test]
    fn invalid_compact_choices_cannot_replace_saved_settings() {
        use crate::settings::{CompactWindow, Settings};
        let directory = tempfile::tempdir().unwrap();
        let store = Store::open(&directory.path().join("usage.sqlite")).unwrap();
        let saved = Settings::default();
        store.save_settings(&saved).unwrap();
        let mut invalid = saved.clone();
        invalid.compact_windows = Some(vec![CompactWindow::CodexFiveHour; 2]);
        assert!(store
            .save_settings(&invalid)
            .unwrap_err()
            .contains("only once"));
        invalid.compact_windows = Some(vec![
            CompactWindow::CodexFiveHour,
            CompactWindow::CodexWeekly,
            CompactWindow::ClaudeFiveHour,
            CompactWindow::ClaudeWeekly,
            CompactWindow::GrokAllowance,
        ]);
        assert!(store
            .save_settings(&invalid)
            .unwrap_err()
            .contains("at most four"));
        assert_eq!(store.settings().unwrap(), Some(saved));
    }

    #[test]
    fn native_claude_quota_survives_restart_and_never_borrows_another_accounts_tokens() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite");
        let mut store = Store::open(&path).unwrap();
        let telemetry = json!({"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"claude-code-desktop"}}]},"scopeLogs":[{"logRecords":[{"body":{"stringValue":"claude_code.api_request"},"attributes":[
            {"key":"user.account_uuid","value":{"stringValue":"one"}}, {"key":"organization.id","value":{"stringValue":"org"}},
            {"key":"session.id","value":{"stringValue":"session-one"}}, {"key":"request_id","value":{"stringValue":"request-one"}},
            {"key":"event.timestamp","value":{"stringValue":"1970-01-01T00:16:40Z"}},
            {"key":"input_tokens","value":{"intValue":"4"}}, {"key":"output_tokens","value":{"intValue":"3"}},
            {"key":"cache_read_tokens","value":{"intValue":"2"}}, {"key":"cache_creation_tokens","value":{"intValue":"1"}}
        ]}]}]}]});
        store.ingest_claude_telemetry(telemetry, 1000).unwrap();
        let quota = json!({"provider":"claude","accountId":"claude:[\"one\",\"org\"]","observedAt":1000,"plan":"pro",
            "quotaObservedAt":1000,"source":"Claude Code account usage","windows":[{"id":"five_hour","remainingPercent":34,"resetsAt":2000}]});
        store.ingest_claude_usage(quota.clone(), 1010).unwrap();
        assert_eq!(
            store.claude_snapshot().unwrap().unwrap()["windows"][0]["remainingPercent"],
            34
        );
        assert_eq!(
            store.claude_snapshot().unwrap().unwrap()["observedTokens"],
            10
        );
        assert_eq!(store.claude_snapshot().unwrap().unwrap()["plan"], "pro");
        let mut other = quota;
        other["accountId"] = json!("claude:[\"other\",\"org\"]");
        other["observedAt"] = json!(1100);
        other["quotaObservedAt"] = json!(1100);
        other["windows"][0]["remainingPercent"] = json!(3);
        store.ingest_claude_usage(other, 1110).unwrap();
        drop(store);
        let reopened = Store::open(&path).unwrap();
        let latest = reopened.claude_snapshot().unwrap().unwrap();
        assert_eq!(latest["accountId"], "claude:[\"other\",\"org\"]");
        assert_eq!(latest["windows"][0]["remainingPercent"], 3);
        assert_eq!(latest["quotaObservedAt"], 1100);
        assert!(latest["observedTokens"].is_null());
        assert_eq!(
            reopened
                .observation_count("claude:[\"one\",\"org\"]")
                .unwrap(),
            1
        );
    }

    #[test]
    fn telemetry_keeps_accounting_detail_and_limit_hits_without_content() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("usage.sqlite")).unwrap();
        let attr = |key: &str, value: Value| json!({"key":key,"value":value});
        let common = || {
            vec![
                attr("user.account_uuid", json!({"stringValue":"one"})),
                attr("organization.id", json!({"stringValue":"org"})),
                attr("session.id", json!({"stringValue":"s"})),
                attr(
                    "event.timestamp",
                    json!({"stringValue":"1970-01-01T00:16:40Z"}),
                ),
            ]
        };
        let mut request = common();
        request.extend([
            attr("event.name", json!({"stringValue":"api_request"})),
            attr("request_id", json!({"stringValue":"r1"})),
            attr("input_tokens", json!({"intValue":"10"})),
            attr("output_tokens", json!({"intValue":"20"})),
            attr("cache_read_tokens", json!({"intValue":"300"})),
            attr("cache_creation_tokens", json!({"intValue":"40"})),
            attr("model", json!({"stringValue":"claude-opus-5-5"})),
            attr("cost_usd", json!({"doubleValue":0.0125})),
            attr("query_source", json!({"stringValue":"subagent"})),
            attr("agent_id", json!({"stringValue":"agent-1"})),
            attr("effort", json!({"stringValue":"high"})),
            attr("ccr.session.id", json!({"stringValue":"cloud-1"})),
            attr("prompt", json!({"stringValue":"private content"})),
        ]);
        let mut limit = common();
        limit.extend([
            attr("event.name", json!({"stringValue":"api_error"})),
            attr("status_code", json!({"intValue":"429"})),
        ]);
        let mut other_error = common();
        other_error.extend([
            attr("event.name", json!({"stringValue":"api_error"})),
            attr("status_code", json!({"intValue":"500"})),
        ]);
        let logs = json!({"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"claude-code"}}]},
            "scopeLogs":[{"logRecords":[{"attributes":request},{"attributes":limit},{"attributes":other_error}]}]}]});
        store.ingest_claude_telemetry(logs.clone(), 1000).unwrap();
        store.ingest_claude_telemetry(logs, 1000).unwrap();
        let row: (String, String, String, String, i64, i64, i64, i64, i64) = store.0.query_row(
            "SELECT surface,model,query_source,agent,input,output,cache_read,cache_write,cost_micros FROM usage_events",
            [], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?,r.get(7)?,r.get(8)?))).unwrap();
        assert_eq!(
            row,
            (
                "cloud".into(),
                "claude-opus-5-5".into(),
                "subagent".into(),
                "agent-1".into(),
                10,
                20,
                300,
                40,
                12500
            )
        );
        let count = |sql: &str| {
            store
                .0
                .query_row(sql, [], |row| row.get::<_, i64>(0))
                .unwrap()
        };
        assert_eq!(count("SELECT COUNT(*) FROM usage_events"), 1);
        assert_eq!(
            count(
                "SELECT COUNT(*) FROM limit_events WHERE kind='rate_limited' AND occurred_at=1000"
            ),
            1
        );
        assert_eq!(count("SELECT COUNT(*) FROM limit_events"), 1);
        let stored: String = store
            .0
            .query_row(
                "SELECT group_concat(COALESCE(model,'')||COALESCE(agent,'')) FROM usage_events",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!stored.contains("private"));
    }

    #[test]
    fn local_log_passes_resume_and_telemetry_replaces_a_transcript_row() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("usage.sqlite")).unwrap();
        let transcript = crate::usage::Request {
            surface: "cli".into(),
            input: 1,
            ..Default::default()
        };
        let scan = crate::local_logs::Scan {
            requests: vec![("claude:r1".into(), 100, transcript.clone())],
            readings: vec![("codex/primary".into(), 100, 12.0, Some(500))],
            cursors: vec![(
                "/logs/a.jsonl".into(),
                crate::local_logs::Cursor {
                    offset: 42,
                    state: json!({"session":"s"}),
                },
            )],
        };
        store
            .ingest_local_logs("claude_transcript", "claude:one", scan)
            .unwrap();
        let cursors = store.log_cursors("claude_transcript").unwrap();
        assert_eq!(cursors["/logs/a.jsonl"].offset, 42);
        assert_eq!(cursors["/logs/a.jsonl"].state["session"], "s");
        assert!(store.log_cursors("codex_rollout").unwrap().is_empty());
        let telemetry = crate::usage::Request {
            cost_micros: Some(9),
            ..transcript.clone()
        };
        insert_usage(&store.0, "claude:one", "otel", "claude:r1", 100, &telemetry).unwrap();
        insert_usage(
            &store.0,
            "claude:one",
            "claude_transcript",
            "claude:r1",
            100,
            &transcript,
        )
        .unwrap();
        let (source, cost): (String, i64) = store
            .0
            .query_row("SELECT source,cost_micros FROM usage_events", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap();
        assert_eq!((source.as_str(), cost), ("otel", 9));
        let readings: i64 = store
            .0
            .query_row("SELECT COUNT(*) FROM limit_readings", [], |row| row.get(0))
            .unwrap();
        assert_eq!(readings, 1);
    }

    fn codex_token_count(total: Value) -> Value {
        json!({"timestamp":"2026-10-30T00:00:10Z","type":"event_msg","payload":{
            "type":"token_count","info":{"total_token_usage":total}
        }})
    }

    #[test]
    fn codex_sqlite_cursor_keeps_only_accounting_and_resume_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rollout.jsonl");
        let mut store = Store::open(&directory.path().join("usage.sqlite")).unwrap();
        let header = json!({"type":"session_meta","payload":{"id":"s1","instructions":"private"}});
        let context =
            json!({"type":"turn_context","payload":{"model":"gpt-5.6-sol","effort":"high"}});
        let count = codex_token_count(
            json!({"input_tokens":12,"cached_input_tokens":3,"output_tokens":4,
            "reasoning_output_tokens":1,"total_tokens":17,"nested":{"text":["private"]}}),
        );
        let log = format!("{header}\n{context}\n{count}\n");
        std::fs::write(&path, &log).unwrap();
        let scan = crate::local_logs::scan(
            crate::local_logs::Kind::Codex,
            directory.path(),
            &Default::default(),
            crate::collector::now(),
        );
        store
            .ingest_local_logs("codex_rollout", "codex:one", scan)
            .unwrap();
        let (offset, state): (i64, String) = store
            .0
            .query_row(
                "SELECT offset,state FROM log_cursors WHERE provider='codex_rollout'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(offset, log.len() as i64);
        assert_eq!(
            serde_json::from_str::<Value>(&state).unwrap(),
            json!({
                "model":"gpt-5.6-sol","effort":"high","session_bytes":[0, header.to_string().len()+1],
                "total":{"input_tokens":12,"cached_input_tokens":3,"output_tokens":4,"reasoning_output_tokens":1}
            })
        );
    }

    #[test]
    fn codex_partial_end_survives_sqlite_round_trip() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rollout.jsonl");
        let db_path = directory.path().join("usage.sqlite");
        let header = json!({"type":"session_meta","payload":{"id":"s1"}});
        let initial = format!("{header}\n{{\"type\":\"partial\"");
        std::fs::write(&path, &initial).unwrap();
        let now = crate::collector::now();
        {
            let mut store = Store::open(&db_path).unwrap();
            let scan = crate::local_logs::scan(
                crate::local_logs::Kind::Codex,
                directory.path(),
                &Default::default(),
                now,
            );
            store
                .ingest_local_logs("codex_rollout", "codex:one", scan)
                .unwrap();
        }
        let store = Store::open(&db_path).unwrap();
        let cursors = store.log_cursors("codex_rollout").unwrap();
        let key = if cfg!(windows) {
            path.to_string_lossy().to_lowercase()
        } else {
            path.to_string_lossy().into_owned()
        };
        assert_eq!(cursors[&key].offset, (header.to_string().len() + 1) as u64);
        assert_eq!(cursors[&key].state["partial_end"], json!(initial.len()));
        let second = crate::local_logs::scan(
            crate::local_logs::Kind::Codex,
            directory.path(),
            &cursors,
            now,
        );
        assert!(second.cursors.is_empty());

        let grown = format!("{initial}}}\n");
        std::fs::write(&path, &grown).unwrap();
        let third = crate::local_logs::scan(
            crate::local_logs::Kind::Codex,
            directory.path(),
            &cursors,
            now,
        );
        assert_eq!(third.cursors.len(), 1);
        assert_eq!(third.cursors[0].1.offset, grown.len() as u64);
        assert!(third.cursors[0].1.state["partial_end"].is_null());
    }

    #[test]
    fn legacy_codex_cursor_is_cleaned_on_read_and_resumes_the_same_session() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rollout.jsonl");
        let store = Store::open(&directory.path().join("usage.sqlite")).unwrap();
        let header = json!({"type":"session_meta","payload":{"id":"s1","instructions":"private"}});
        let prefix = format!("{{}}\n{header}\npartial line:");
        let legacy = json!({"session":"s1","skip":true,"model":{"prompt":"private"},"effort":"high",
            "total":{"input_tokens":12,"cached_input_tokens":3,"output_tokens":4,
            "reasoning_output_tokens":{"text":"private"},"nested":{"text":"private"}},"extra":"private"});
        store
            .0
            .execute(
                "INSERT INTO log_cursors VALUES ('codex_rollout',?1,?2,?3)",
                params![
                    path.to_string_lossy(),
                    prefix.len() as i64,
                    legacy.to_string()
                ],
            )
            .unwrap();
        let cursors = store.log_cursors("codex_rollout").unwrap();
        let cursor = &cursors[path.to_string_lossy().as_ref()];
        assert_eq!(cursor.offset, prefix.len() as u64);
        assert_eq!(
            cursor.state,
            json!({"skip":true,"effort":"high","session_bytes":[0,prefix.len()],
            "total":{"input_tokens":12,"cached_input_tokens":3,"output_tokens":4,"reasoning_output_tokens":0}})
        );
        let persisted: String = store
            .0
            .query_row("SELECT state FROM log_cursors", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&persisted).unwrap(),
            cursor.state
        );

        let skipped = codex_token_count(json!({"input_tokens":50}));
        let count = codex_token_count(
            json!({"input_tokens":18,"cached_input_tokens":4,"output_tokens":6,
            "reasoning_output_tokens":1}),
        );
        std::fs::write(&path, format!("{prefix}{skipped}\n{count}\n")).unwrap();
        let scan = crate::local_logs::scan(
            crate::local_logs::Kind::Codex,
            directory.path(),
            &cursors,
            crate::collector::now(),
        );
        assert_eq!(scan.requests.len(), 1);
        let (key, _, request) = &scan.requests[0];
        assert_eq!(key, "codex:s1:29");
        assert_eq!(
            (
                request.input,
                request.cache_read,
                request.output,
                request.reasoning
            ),
            (5, 1, 2, Some(1))
        );
        assert_eq!(request.model, None);
        assert_eq!(request.effort.as_deref(), Some("high"));
    }

    #[test]
    fn upload_pages_bound_complete_json_and_resume_at_the_last_sent_row() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("usage.sqlite")).unwrap();
        let device = "d".repeat(43);
        let now = crate::collector::now() as i64;
        let request = crate::usage::Request {
            surface: "cli".into(),
            model: Some("燃\"\\\n".repeat(32)),
            ..Default::default()
        };
        let keys: Vec<_> = (0..200).map(|i| format!("local-{i}")).collect();
        let tx = store.0.transaction().unwrap();
        for key in &keys {
            insert_usage(&tx, "claude:other", "relay", key, now, &request).unwrap();
            insert_usage(&tx, "claude:one", "otel", key, now, &request).unwrap();
        }
        tx.commit().unwrap();
        let mut sent = Vec::new();
        for _ in 0..5 {
            let (through, rows) = store.unshared_usage("chan", &device, 200).unwrap();
            if rows.is_empty() {
                break;
            }
            assert!(serde_json::to_vec(&json!({ "rows": rows })).unwrap().len() <= 100_000);
            assert!(rows.len() < 200);
            assert_eq!(
                store.unshared_usage("chan", &device, 200).unwrap(),
                (through, rows.clone())
            );
            sent.extend(
                rows.iter()
                    .map(|row| row["key"].as_str().unwrap().to_owned()),
            );
            store.mark_shared("chan", through).unwrap();
        }
        assert_eq!(sent, keys);
    }

    #[test]
    fn failed_download_rolls_back_rows_and_cursor_until_retry() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("usage.sqlite")).unwrap();
        let rows = [(
            3,
            (
                "claude:one".into(),
                "remote".into(),
                123,
                Default::default(),
            ),
        )];
        let devices = [crate::usage_relay::Announced {
            origin: "d".repeat(43),
            name: "Remote".into(),
            details: String::new(),
            at: 124,
        }];
        let ingest = |store: &mut Store| store.ingest_relay("chan", &rows, &[], &devices, 7);
        let count = |store: &Store| {
            store
                .0
                .query_row("SELECT COUNT(*) FROM usage_events", [], |r| {
                    r.get::<_, i64>(0)
                })
                .unwrap()
        };
        store.0.execute_batch("CREATE TRIGGER reject_device BEFORE INSERT ON devices BEGIN SELECT RAISE(ABORT,'fixture'); END").unwrap();
        assert!(ingest(&mut store).is_err());
        assert_eq!(store.relay_cursor("chan").unwrap(), 0);
        assert_eq!(count(&store), 0);
        store.0.execute_batch("DROP TRIGGER reject_device").unwrap();
        ingest(&mut store).unwrap();
        assert_eq!(store.relay_cursor("chan").unwrap(), 7);
        assert_eq!(count(&store), 1);
    }

    #[test]
    fn usage_channel_sends_local_rows_once_and_never_echoes_received_rows() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("usage.sqlite")).unwrap();
        let request = crate::usage::Request {
            surface: "cli".into(),
            input: 1,
            ..Default::default()
        };
        let now = crate::collector::now() as i64;
        insert_usage(
            &store.0,
            "claude:one",
            "otel",
            "old",
            now - 15 * DAY as i64,
            &request,
        )
        .unwrap();
        for key in ["a", "b", "c"] {
            insert_usage(&store.0, "claude:one", "otel", key, now, &request).unwrap();
        }
        let (through, rows) = store.unshared_usage("chan", "here", 2).unwrap();
        assert_eq!(rows.len(), 2);
        store.mark_shared("chan", through).unwrap();
        let cloud = crate::usage::Request {
            surface: "cloud".into(),
            ..request.clone()
        };
        store
            .ingest_relay(
                "chan",
                &[(9, ("claude:one".into(), "cloud-1".into(), 120, cloud))],
                &[],
                &[],
                9,
            )
            .unwrap();
        assert_eq!(store.relay_cursor("chan").unwrap(), 9);
        let (_, rest) = store.unshared_usage("chan", "here", 10).unwrap();
        assert_eq!(
            rest.iter()
                .map(|row| row["key"].as_str().unwrap())
                .collect::<Vec<_>>(),
            ["c"]
        );
        assert_eq!(
            store.unshared_usage("other", "here", 10).unwrap().1.len(),
            3
        );
        assert_eq!(store.relay_cursor("other").unwrap(), 0);
    }

    #[test]
    fn plan_backfill_keeps_recorded_history_and_fills_each_untracked_account() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("plans.sqlite");
        let mut store = Store::open(&path).unwrap();
        for (at, plan) in [(200, "pro"), (210, "plus")] {
            store
                .save(&json!({"accountId":"tracked","observedAt":at,"plan":plan}))
                .unwrap();
        }
        for account in ["tracked", "legacy"] {
            for (at, plan) in [(100, "plus"), (110, ""), (120, "plus"), (130, "team")] {
                store
                    .0
                    .execute(
                        "INSERT INTO observations VALUES (?1,?2,?3)",
                        params![account, at, json!({"plan":plan}).to_string()],
                    )
                    .unwrap();
            }
        }
        drop(store);
        for _ in 0..2 {
            let store = Store::open(&path).unwrap();
            for (account, at, text) in [
                ("tracked", 210, "Plan changed from pro to plus"),
                ("legacy", 130, "Plan changed from plus to team"),
            ] {
                assert_eq!(
                    store.ledger_marks(account, 0).unwrap(),
                    [json!({"at":at,"kind":"plan","source":"auto","text":text})],
                    "{account}"
                );
            }
        }
    }

    #[test]
    fn usage_channel_sends_new_rows_after_all_uploaded_events_are_pruned() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite");
        let mut store = Store::open(&path).unwrap();
        let request = crate::usage::Request {
            surface: "cli".into(),
            input: 1,
            ..Default::default()
        };
        let now = crate::collector::now();
        insert_usage(&store.0, "claude:one", "otel", "old", now as i64, &request).unwrap();
        let (through, rows) = store.unshared_usage("chan", "here", 10).unwrap();
        assert_eq!(rows[0]["key"], "old");
        store.mark_shared("chan", through).unwrap();
        let later = now + 91 * DAY;
        store.prune(later).unwrap();
        let raw_rows: i64 = store
            .0
            .query_row("SELECT count(*) FROM usage_events", [], |row| row.get(0))
            .unwrap();
        assert_eq!(raw_rows, 0);
        drop(store);

        let mut store = Store::open(&path).unwrap();
        insert_usage(
            &store.0,
            "claude:one",
            "otel",
            "new",
            later as i64,
            &request,
        )
        .unwrap();
        let (next, rows) = store.unshared_usage("chan", "here", 10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["key"], "new");
        assert!(next > through);
        store.mark_shared("chan", next).unwrap();
        store.mark_shared("chan", through).unwrap();
        assert!(store
            .unshared_usage("chan", "here", 10)
            .unwrap()
            .1
            .is_empty());
    }

    #[test]
    fn legacy_usage_upload_positions_survive_migration_and_new_inserts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite");
        let legacy = Connection::open(&path).unwrap();
        legacy.execute_batch(r#"CREATE TABLE usage_events (account TEXT NOT NULL, source TEXT NOT NULL, key TEXT NOT NULL, occurred_at INTEGER NOT NULL,
            surface TEXT NOT NULL, model TEXT, effort TEXT, speed TEXT, query_source TEXT, agent TEXT, input INTEGER NOT NULL, output INTEGER NOT NULL,
            cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL, reasoning INTEGER, cost_micros INTEGER, origin TEXT, PRIMARY KEY(account,key));
            CREATE INDEX usage_events_time ON usage_events(account,occurred_at);
            CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            INSERT INTO settings VALUES ('usage-relay:chan','{"pushed":4,"seq":17}'),
                ('usage-relay:other','{"pushed":15,"seq":23}'), ('usage-relay:invalid','{');"#).unwrap();
        let now = crate::collector::now() as i64;
        for (id, source, key, origin) in [
            (3, "otel", "sent", None),
            (7, "otel", "pending", None),
            (8, "relay", "received", Some("other-computer")),
        ] {
            legacy.execute("INSERT INTO usage_events (rowid,account,source,key,occurred_at,surface,input,output,cache_read,cache_write,origin)
                VALUES (?1,'claude:one',?2,?3,?4,'cli',1,0,0,0,?5)", params![id, source, key, now, origin]).unwrap();
        }
        drop(legacy);

        let store = Store::open(&path).unwrap();
        let (through, pending) = store.unshared_usage("chan", "here", 10).unwrap();
        assert_eq!(through, 7);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["key"], "pending");
        assert_eq!(store.relay_cursor("chan").unwrap(), 17);
        assert_eq!(store.relay_cursor("other").unwrap(), 23);
        assert_eq!(store.computers().unwrap()["others"][0]["lastUsage"], now);

        let request = crate::usage::Request {
            surface: "cli".into(),
            input: 1,
            ..Default::default()
        };
        insert_usage(&store.0, "claude:one", "otel", "new", now, &request).unwrap();
        let (next, rows) = store.unshared_usage("other", "here", 10).unwrap();
        assert!(next > 15);
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["key"], "new");
    }

    #[test]
    fn plan_changes_and_notes_mark_the_ledger() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite");
        let mut store = Store::open(&path).unwrap();
        let now = crate::collector::now() as i64;
        let snapshot = |at: i64, plan: &str| json!({"accountId":"codex:one","observedAt":at,"plan":plan,"windows":[]});
        // Observations saved before plan history existed are backfilled on open.
        store.save(&snapshot(now - 3 * 86_400, "pro")).unwrap();
        store.save(&snapshot(now - 2 * 86_400, "plus")).unwrap();
        store.0.execute("DELETE FROM ledger_marks", []).unwrap();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        store.save(&snapshot(now - 86_400, "plus")).unwrap();
        store.save(&snapshot(now - 3600, "pro")).unwrap();
        store
            .add_ledger_mark("codex:one", now - 86_400, false, " Started a big refactor ")
            .unwrap();
        assert!(store.add_ledger_mark("codex:one", now, true, " ").is_err());
        let marks = store.ledger_marks("codex:one", now - 30 * 86_400).unwrap();
        let texts: Vec<&str> = marks
            .iter()
            .map(|mark| mark["text"].as_str().unwrap())
            .collect();
        assert_eq!(
            texts,
            [
                "Plan changed from pro to plus",
                "Started a big refactor",
                "Plan changed from plus to pro"
            ]
        );
        store
            .remove_ledger_mark("codex:one", now - 86_400, "note")
            .unwrap();
        assert_eq!(store.ledger_marks("codex:one", 0).unwrap().len(), 2);
    }

    #[test]
    fn usage_list_filters_pages_and_shows_allowance_steps() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("usage.sqlite")).unwrap();
        let now = 1_800_000_000_u64;
        let at = now as i64 - 3600;
        for index in 0..150 {
            let request = crate::usage::Request {
                surface: if index % 2 == 0 { "cli" } else { "desktop" }.into(),
                model: Some("claude-opus-5-5".into()),
                query_source: Some("main".into()),
                input: index,
                ..Default::default()
            };
            insert_usage(
                &store.0,
                "claude:one",
                "otel",
                &format!("r{index}"),
                at + index,
                &request,
            )
            .unwrap();
        }
        store
            .save(&json!({"accountId":"claude:one","observedAt":at,"windows":[{"id":"seven_day","usedPercent":40.0,"remainingPercent":60.0,"resetsAt":now+9000}]}))
            .unwrap();
        store
            .save(&json!({"accountId":"claude:one","observedAt":at+100,"windows":[{"id":"seven_day","usedPercent":41.0,"remainingPercent":59.0,"resetsAt":now+9000}]}))
            .unwrap();
        let list =
            |filter: UsageFilter| store.usage_list("claude:one", &filter, now, false).unwrap();
        let all = list(UsageFilter {
            days: 7,
            ..Default::default()
        });
        assert_eq!(all["total"], 150);
        assert_eq!(all["rows"].as_array().unwrap().len(), 100);
        assert_eq!(all["rows"][0]["input"], 149);
        assert_eq!(all["choices"]["where"], json!(["cli", "desktop"]));
        assert_eq!(all["steps"][0]["used"], 41.0);
        let second = list(UsageFilter {
            days: 7,
            page: 1,
            ..Default::default()
        });
        assert_eq!(second["rows"].as_array().unwrap().len(), 50);
        let cli = list(UsageFilter {
            days: 7,
            place: "cli".into(),
            ..Default::default()
        });
        assert_eq!(cli["total"], 75);
        let day = time::OffsetDateTime::from_unix_timestamp(at)
            .unwrap()
            .date()
            .to_string();
        assert_eq!(
            list(UsageFilter {
                date: day,
                ..Default::default()
            })["total"],
            150
        );
        assert!(store
            .usage_list(
                "claude:one",
                &UsageFilter {
                    date: "nope".into(),
                    ..Default::default()
                },
                now,
                false
            )
            .is_err());
        let csv = store
            .usage_list(
                "claude:one",
                &UsageFilter {
                    days: 7,
                    ..Default::default()
                },
                now,
                true,
            )
            .unwrap();
        let text = csv["csv"].as_str().unwrap();
        assert_eq!(text.lines().count(), 151);
        assert!(text
            .lines()
            .nth(2)
            .unwrap()
            .contains(",\"cli\",\"claude-opus-5-5\",\"main\",148,"));
    }

    #[test]
    fn ledger_range_is_saved_and_limited_to_offered_choices() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite");
        let store = Store::open(&path).unwrap();
        assert_eq!(store.ledger_days(), 28);
        assert!(store.save_ledger_days(30).is_err());
        store.save_ledger_days(90).unwrap();
        drop(store);
        assert_eq!(Store::open(&path).unwrap().ledger_days(), 90);
    }

    #[test]
    fn ledger_marks_usage_shared_by_other_computers() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("usage.sqlite")).unwrap();
        let now = crate::collector::now();
        let request = |surface: &str| crate::usage::Request {
            surface: surface.into(),
            input: 1,
            ..Default::default()
        };
        insert_usage(
            &store.0,
            "claude:one",
            "otel",
            "here",
            now as i64,
            &request("cli"),
        )
        .unwrap();
        store
            .ingest_relay(
                "chan",
                &[
                    (
                        1,
                        (
                            "claude:one".into(),
                            "mac".into(),
                            now as i64,
                            request("cli"),
                        ),
                    ),
                    (
                        2,
                        (
                            "claude:one".into(),
                            "web".into(),
                            now as i64,
                            request("cloud"),
                        ),
                    ),
                ],
                &[("claude:one".into(), "mac".into(), "o".repeat(43))],
                &[],
                2,
            )
            .unwrap();
        let ledger = store
            .consistency(
                crate::burn::Provider::Claude,
                &json!({"accountId":"claude:one","windows":[]}),
                now,
                28,
            )
            .unwrap();
        let mut surfaces: Vec<_> = ledger["surfaces"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["surface"].as_str().unwrap().to_owned())
            .collect();
        surfaces.sort();
        assert_eq!(surfaces, ["cli", "cloud", "other:cli"]);

        // Once the sending computer announces itself, its usage carries its name.
        store
            .ingest_relay(
                "chan",
                &[],
                &[],
                &[crate::usage_relay::Announced {
                    origin: "o".repeat(43),
                    name: "Studio Mac".into(),
                    details: "m-dash 0.6.0;logs=1;feed=1".into(),
                    at: now as i64,
                }],
                3,
            )
            .unwrap();
        let ledger = store
            .consistency(
                crate::burn::Provider::Claude,
                &json!({"accountId":"claude:one","windows":[]}),
                now,
                28,
            )
            .unwrap();
        assert!(ledger["surfaces"]
            .as_array()
            .unwrap()
            .iter()
            .any(|row| row["surface"] == "other:cli@Studio Mac"));
        let review = store
            .away_usage(
                crate::burn::Provider::Claude,
                &json!({"accountId":"claude:one","windows":[]}),
                now,
                now as i64 - 60..=now as i64,
            )
            .unwrap();
        let sources: Vec<_> = review["observed"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| row["surface"].as_str().unwrap())
            .collect();
        assert_eq!(sources, ["cli", "cloud", "other:cli@Studio Mac"]);
        assert_eq!(review["account"], "claude:one");
        let computers = store.computers().unwrap();
        assert_eq!(computers["others"][0]["name"], "Studio Mac");
        assert_eq!(computers["others"][0]["lastUsage"], now as i64);
        assert_eq!(computers["cloudLastUsage"], now as i64);
    }

    #[test]
    fn allowance_readings_and_usage_rollups_outlive_raw_history() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("usage.sqlite")).unwrap();
        store
            .save(
                &json!({"accountId":"codex-one","observedAt":1000,"windows":[
                {"id":"codex/primary","usedPercent":41.0,"remainingPercent":59.0,"resetsAt":9000},
                {"id":"legacy","remainingPercent":75.0}]}),
            )
            .unwrap();
        let request = crate::usage::Request {
            surface: "cli".into(),
            model: Some("m".into()),
            input: 5,
            output: 7,
            cost_micros: Some(3),
            ..Default::default()
        };
        for (key, at) in [("a", 100), ("b", 200), ("c", 95 * DAY as i64)] {
            insert_usage(&store.0, "codex-one", "test", key, at, &request).unwrap();
        }
        store.prune(100 * DAY).unwrap();
        let count = |store: &Store, sql: &str| {
            store
                .0
                .query_row(sql, [], |row| row.get::<_, i64>(0))
                .unwrap()
        };
        assert_eq!(count(&store, "SELECT COUNT(*) FROM observations"), 0);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM limit_readings"), 2);
        assert_eq!(
            count(
                &store,
                "SELECT CAST(used_percent AS INTEGER) FROM limit_readings WHERE window='legacy'"
            ),
            25
        );
        assert_eq!(count(&store, "SELECT COUNT(*) FROM usage_events"), 1);
        assert_eq!(
            count(&store, "SELECT requests*1000+input FROM usage_daily WHERE day='1970-01-01' AND model='m' AND query_source=''"),
            2010
        );
        store.prune(500 * DAY).unwrap();
        assert_eq!(count(&store, "SELECT COUNT(*) FROM limit_readings"), 0);
        assert_eq!(count(&store, "SELECT COUNT(*) FROM usage_daily"), 2);
    }

    #[test]
    fn raw_observations_and_allowance_readings_expire_at_their_own_cutoffs() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let at = 2 * DAY;
        for time in [at - 1, at, at + 1] {
            store
                .save(&json!({"accountId":"one","observedAt":time,
                "windows":[{"id":"weekly","usedPercent":25}]}))
                .unwrap();
        }
        for (age, observations, readings) in [
            (30 * DAY - 1, 3, 3),
            (30 * DAY, 2, 3),
            (30 * DAY + 1, 1, 3),
            (400 * DAY, 0, 2),
            (400 * DAY + 1, 0, 1),
            (400 * DAY + 2, 0, 0),
        ] {
            store.prune(at + age).unwrap();
            assert_eq!(
                store.observation_count("one").unwrap(),
                observations,
                "age={age}"
            );
            let kept: i64 = store
                .0
                .query_row("SELECT COUNT(*) FROM limit_readings", [], |row| row.get(0))
                .unwrap();
            assert_eq!(kept, readings, "age={age}");
        }
        assert_eq!(
            store.latest("codex").unwrap().unwrap()["observedAt"],
            at + 1
        );
    }

    #[test]
    fn pruning_at_midday_rolls_up_only_complete_utc_days_once() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let midnight = 2 * DAY;
        let tx = store.0.transaction().unwrap();
        for (at, key, tokens) in [
            (midnight - 1, "previous", 7),
            (midnight, "start", 11),
            (midnight + 1, "next", 13),
        ] {
            let at = at as i64;
            insert_claude_tokens(&tx, "one", key, at, tokens).unwrap();
            let request = crate::usage::Request {
                surface: "cli".into(),
                input: tokens,
                ..Default::default()
            };
            insert_usage(&tx, "one", "otel", key, at, &request).unwrap();
        }
        tx.commit().unwrap();
        for (days, expected) in [
            (35, (24, 31, 31, 0)),
            (90, (0, 31, 24, 7)),
            (91, (0, 31, 0, 31)),
        ] {
            for _ in 0..2 {
                store.prune(midnight + days * DAY + DAY / 2).unwrap();
                let totals: (i64, i64, i64, i64) = store
                    .0
                    .query_row(
                        "SELECT (SELECT COALESCE(SUM(tokens),0) FROM claude_requests),
                            (SELECT COALESCE(SUM(tokens),0) FROM daily_tokens),
                            (SELECT COALESCE(SUM(input),0) FROM usage_events),
                            (SELECT COALESCE(SUM(input),0) FROM usage_daily)",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
                    )
                    .unwrap();
                assert_eq!(totals, expected, "days={days}");
            }
        }
        let mut snapshot = json!({"accountId":"one"});
        store.attach_history(&mut snapshot).unwrap();
        assert_eq!(
            snapshot["dailyUsage"],
            json!([
                {"date":"1970-01-02","tokens":7}, {"date":"1970-01-03","tokens":24}
            ])
        );
    }

    fn claude_request(request: &str, at: &str, session: &str) -> Value {
        json!({"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"claude-code"}}]},
            "scopeLogs":[{"logRecords":[{"body":{"stringValue":"claude_code.api_request"},"attributes":[
            {"key":"user.account_uuid","value":{"stringValue":"one"}}, {"key":"organization.id","value":{"stringValue":"org"}},
            {"key":"session.id","value":{"stringValue":session}}, {"key":"request_id","value":{"stringValue":request}},
            {"key":"event.timestamp","value":{"stringValue":at}}, {"key":"input_tokens","value":{"intValue":"10"}},
            {"key":"output_tokens","value":{"intValue":"5"}},
            {"key":"cache_read_tokens","value":{"intValue":"0"}}, {"key":"cache_creation_tokens","value":{"intValue":"0"}}]}]}]}]})
    }

    #[test]
    fn pruned_day_counts_replays_and_late_requests_once_across_restarts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("usage.sqlite");
        let mut store = Store::open(&path).unwrap();
        let account = r#"claude:["one","org"]"#;
        let event = |key| claude_request(key, "1970-01-01T00:16:40Z", "s");
        for key in ["first", "second"] {
            store.ingest_claude_telemetry(event(key), 1000).unwrap();
        }
        let assert_tokens = |store: &Store, tokens: i64| {
            let snapshot = store.claude_snapshot().unwrap().unwrap();
            assert_eq!(snapshot["observedTokens"], tokens);
            assert_eq!(
                snapshot["dailyUsage"],
                json!([{"date":"1970-01-01","tokens":tokens}])
            );
            assert_eq!(snapshot["observedSince"], 0);
            assert_eq!(
                store.latest("claude").unwrap().unwrap()["dailyUsage"],
                snapshot["dailyUsage"]
            );
        };
        store.prune(40 * DAY).unwrap();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        assert_tokens(&store, 30);
        store
            .ingest_claude_telemetry(event("first"), 40 * DAY)
            .unwrap();
        assert_tokens(&store, 30);
        store.prune(100 * DAY).unwrap();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        let redated = claude_request("first", "1970-01-02T00:16:40Z", "s");
        store.ingest_claude_telemetry(redated, 100 * DAY).unwrap();
        assert_tokens(&store, 30);
        for (key, total) in [("second", 30), ("late", 45), ("late", 45)] {
            store
                .ingest_claude_telemetry(event(key), 100 * DAY)
                .unwrap();
            assert_tokens(&store, total);
        }
        for _ in 0..2 {
            store.prune(100 * DAY).unwrap();
        }
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_tokens(&store, 45);
        let rollup: (i64, i64, i64) = store.0.query_row(
            "SELECT requests,input,output FROM usage_daily WHERE account=?1 AND day='1970-01-01'",
            [account], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
        assert_eq!(rollup, (3, 30, 15));
    }

    #[test]
    fn failed_rollup_keeps_the_late_row_and_its_key_available_for_retry() {
        let mut store = Store::open(Path::new(":memory:")).unwrap();
        let account = "claude:fixture";
        let mut request = crate::usage::Request {
            surface: "cli".into(),
            input: 5,
            cost_micros: Some(7),
            ..Default::default()
        };
        insert_usage(&store.0, account, "otel", "first", 100, &request).unwrap();
        store.prune(100 * DAY).unwrap();
        request.cost_micros = None;
        insert_usage(
            &store.0,
            account,
            "claude_transcript",
            "late",
            200,
            &request,
        )
        .unwrap();
        store
            .0
            .execute_batch(
                "CREATE TEMP TRIGGER fail_rollup BEFORE INSERT ON usage_daily
            BEGIN SELECT RAISE(ABORT,'fixture write failure'); END;",
            )
            .unwrap();
        assert!(store.prune(100 * DAY).is_err());
        assert_eq!(
            store
                .usage_list(account, &UsageFilter::default(), 200, false)
                .unwrap()["total"],
            1
        );
        store.0.execute_batch("DROP TRIGGER fail_rollup").unwrap();
        request.reasoning = Some(2);
        insert_usage(&store.0, account, "otel", "late", 200, &request).unwrap();
        store.prune(100 * DAY).unwrap();
        let rollup: (i64, i64, i64, i64) = store
            .0
            .query_row(
                "SELECT requests,input,reasoning,cost_micros FROM usage_daily WHERE account=?1",
                [account],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(rollup, (2, 10, 2, 7));
    }

    fn legacy_usage_db(path: &Path) -> Connection {
        let legacy = Connection::open(path).unwrap();
        legacy.execute_batch("CREATE TABLE daily_tokens (account TEXT, day TEXT, tokens INTEGER, PRIMARY KEY(account,day));
                CREATE TABLE claude_requests (account TEXT, request TEXT, occurred_at INTEGER, tokens INTEGER, PRIMARY KEY(account,request));
                CREATE TABLE usage_daily (account TEXT, day TEXT, surface TEXT, model TEXT, query_source TEXT, requests INTEGER,
                    input INTEGER, output INTEGER, cache_read INTEGER, cache_write INTEGER, reasoning INTEGER, cost_micros INTEGER,
                    PRIMARY KEY(account,day,surface,model,query_source));
                CREATE TABLE usage_events (account TEXT, source TEXT, key TEXT, occurred_at INTEGER, surface TEXT, model TEXT,
                    effort TEXT, speed TEXT, query_source TEXT, agent TEXT, input INTEGER, output INTEGER, cache_read INTEGER,
                    cache_write INTEGER, reasoning INTEGER, cost_micros INTEGER, PRIMARY KEY(account,key));").unwrap();
        legacy
    }

    #[test]
    fn upgrading_preserves_unkeyed_archives_and_counts_live_requests() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("old.sqlite");
        let account = r#"claude:["one","org"]"#;
        {
            let legacy = legacy_usage_db(&path);
            // One redelivered request has a revised count. The other day's request is still live.
            legacy
                .execute(
                    "INSERT INTO daily_tokens VALUES (?1,'1970-01-01',30)",
                    [account],
                )
                .unwrap();
            legacy
                .execute(
                    "INSERT INTO claude_requests VALUES (?1,'first',1000,45),(?1,'live',87400,15)",
                    [account],
                )
                .unwrap();
            legacy.execute("INSERT INTO usage_daily VALUES (?1,'1970-01-01','cli','','',2,20,10,0,0,NULL,12)", [account]).unwrap();
            legacy.execute("INSERT INTO usage_events VALUES (?1,'otel','claude:first',1000,'cli',NULL,NULL,NULL,NULL,NULL,40,5,0,0,NULL,6)", [account]).unwrap();
        }
        let mut store = Store::open(&path).unwrap();
        store
            .save(
                &json!({"provider":"claude","accountId":account,"observedAt":1000,
            "dailyUsage":[{"date":"1970-01-01","tokens":15}]}),
            )
            .unwrap();
        let replay = claude_request("second", "1970-01-01T00:16:40Z", "s");
        store.ingest_claude_telemetry(replay, 100 * DAY).unwrap();
        assert_eq!(
            store.claude_snapshot().unwrap().unwrap()["observedTokens"],
            45
        );
        let new = claude_request("new", "1970-01-02T00:16:40Z", "s");
        store
            .ingest_claude_telemetry(new.clone(), 100 * DAY)
            .unwrap();
        store.prune(100 * DAY).unwrap();
        drop(store);
        let mut store = Store::open(&path).unwrap();
        let snapshot = store
            .ingest_claude_telemetry(new, 100 * DAY)
            .unwrap()
            .unwrap();
        assert_eq!(
            snapshot["dailyUsage"],
            json!([{"date":"1970-01-01","tokens":30},{"date":"1970-01-02","tokens":30}])
        );
        let rollup: (i64,i64) = store.0.query_row("SELECT requests,cost_micros FROM usage_daily WHERE account=?1 AND day='1970-01-01'",
            [account], |row| Ok((row.get(0)?,row.get(1)?))).unwrap();
        assert_eq!(rollup, (2, 12));
    }

    #[test]
    fn upgrading_preserves_a_partly_replayed_40_day_total_before_usage_rollup() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.sqlite");
        let account = r#"claude:["one","org"]"#;
        let now = 40 * DAY;
        {
            let legacy = legacy_usage_db(&path);
            legacy
                .execute_batch(
                    "CREATE TABLE settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
                INSERT INTO settings VALUES ('claude-current-account','claude:other');",
                )
                .unwrap();
            legacy
                .execute(
                    "INSERT INTO daily_tokens VALUES (?1,'1970-01-01',30)",
                    [account],
                )
                .unwrap();
            for key in ["claude:first", "claude:second"] {
                legacy.execute("INSERT INTO usage_events VALUES (?1,'otel',?2,1000,'cli',NULL,NULL,NULL,NULL,NULL,10,5,0,0,NULL,NULL)",
                    params![account, key]).unwrap();
            }
            // After the 35-day prune, only the revised delivery has a live token row.
            legacy
                .execute(
                    "INSERT INTO claude_requests VALUES (?1,'first',1000,45)",
                    [account],
                )
                .unwrap();
        }
        let assert_saved = |store: &Store| {
            assert_eq!(
                store.claude_snapshot().unwrap().unwrap()["accountId"],
                "claude:other"
            );
            let mut snapshot = json!({"accountId":account});
            store.attach_history(&mut snapshot).unwrap();
            assert_eq!(
                snapshot["dailyUsage"],
                json!([{"date":"1970-01-01","tokens":30}])
            );
        };
        let mut store = Store::open(&path).unwrap();
        assert_saved(&store);
        for key in ["first", "second"] {
            store
                .ingest_claude_telemetry(claude_request(key, "1970-01-01T00:16:40Z", "s"), now)
                .unwrap();
            assert_saved(&store);
        }
        store.prune(now).unwrap();
        drop(store);
        assert_saved(&Store::open(&path).unwrap());
    }

    #[test]
    fn upgrading_rebuilds_a_saved_total_only_when_the_request_keys_match() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("legacy.sqlite");
        {
            let legacy = legacy_usage_db(&path);
            for (account, second_key) in [
                ("claude:complete", "claude:new"),
                ("claude:partial", "claude:missing"),
            ] {
                legacy
                    .execute(
                        "INSERT INTO daily_tokens VALUES (?1,'1970-01-01',15)",
                        [account],
                    )
                    .unwrap();
                legacy.execute("INSERT INTO claude_requests VALUES (?1,'first',1000,15),(?1,'new',1001,15)", [account]).unwrap();
                for key in ["claude:first", second_key] {
                    legacy.execute("INSERT INTO usage_events VALUES (?1,'otel',?2,1000,'cli',NULL,NULL,NULL,NULL,NULL,10,5,0,0,NULL,NULL)",
                        params![account, key]).unwrap();
                }
            }
        }
        let store = Store::open(&path).unwrap();
        for (account, total) in [("claude:complete", 30), ("claude:partial", 15)] {
            let mut snapshot = json!({"accountId":account});
            store.attach_history(&mut snapshot).unwrap();
            assert_eq!(
                snapshot["dailyUsage"],
                json!([{"date":"1970-01-01","tokens":total}]),
                "{account}"
            );
        }
    }

    #[test]
    fn pruning_keeps_claude_daily_totals_and_drops_only_old_detail() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("prune.sqlite")).unwrap();
        let event = claude_request;
        let recent = 40 * DAY + 100;
        store
            .ingest_claude_telemetry(event("old", "1970-01-01T00:16:40Z", "old-session"), 1000)
            .unwrap();
        store
            .ingest_claude_telemetry(event("new", "1970-02-10T00:01:40Z", "new-session"), recent)
            .unwrap();
        for at in [1000, recent] {
            store
                .save(&json!({"accountId":"codex","observedAt":at,"windows":[]}))
                .unwrap();
        }
        let before = store.claude_snapshot().unwrap().unwrap();
        store.prune(41 * DAY).unwrap();
        let after = store.claude_snapshot().unwrap().unwrap();
        for field in ["observedTokens", "lastTokenAt", "dailyUsage", "accountId"] {
            assert_eq!(after[field], before[field], "{field} changed after pruning");
        }
        assert_eq!(after["observedTokens"], 30);
        // A pruned day keeps its total but not its first request time.
        assert_eq!(after["observedSince"], 0);
        assert_eq!(
            after["dailyUsage"],
            json!([{"date":"1970-01-01","tokens":15},{"date":"1970-02-10","tokens":15}])
        );
        let count = |sql: &str| {
            store
                .0
                .query_row(sql, [], |row| row.get::<_, i64>(0))
                .unwrap()
        };
        assert_eq!(count("SELECT COUNT(*) FROM claude_requests"), 1);
        assert_eq!(count("SELECT COUNT(*) FROM claude_sessions"), 1);
        assert_eq!(store.observation_count("codex").unwrap(), 1);
        store
            .ingest_claude_telemetry(
                event("newer", "1970-02-10T00:05:00Z", "new-session"),
                recent + 300,
            )
            .unwrap();
        store.prune(41 * DAY).unwrap();
        let later = store.claude_snapshot().unwrap().unwrap();
        assert_eq!(later["observedTokens"], 45);
        assert_eq!(later["observedSince"], 0);
    }

    #[test]
    fn claude_replays_do_not_recount_requests_or_make_cached_quota_fresh() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("claude.sqlite");
        let mut store = Store::open(&path).unwrap();
        let status = json!({"session_id":"session-one","cost":{"total_api_duration_ms":1200},
            "rate_limits":{"five_hour":{"used_percentage":25,"resets_at":20000}}});
        store
            .ingest_claude_statusline(status.clone(), 1000)
            .unwrap();
        assert!(
            store.claude_snapshot().unwrap().is_none(),
            "Unidentified quota must not enter account history"
        );
        let telemetry = json!({"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"claude-code"}}]},
        "scopeLogs":[{"logRecords":[{"body":{"stringValue":"claude_code.api_request"},"attributes":[
            {"key":"event.name","value":{"stringValue":"api_request"}},
            {"key":"user.account_uuid","value":{"stringValue":"account-one"}},
            {"key":"organization.id","value":{"stringValue":"org-one"}},
            {"key":"session.id","value":{"stringValue":"session-one"}},
            {"key":"request_id","value":{"stringValue":"request-one"}},
            {"key":"event.timestamp","value":{"stringValue":"1970-01-01T00:16:40Z"}},
            {"key":"input_tokens","value":{"intValue":"100"}},
            {"key":"cache_read_tokens","value":{"intValue":"400"}},
            {"key":"cache_creation_tokens","value":{"intValue":"50"}},
            {"key":"output_tokens","value":{"intValue":"150"}},
            {"key":"prompt","value":{"stringValue":"Do not retain this private content"}}
        ]}]}]}]});
        store
            .ingest_claude_telemetry(telemetry.clone(), 1001)
            .unwrap();
        store.ingest_claude_telemetry(telemetry, 1300).unwrap();
        let mut cached_status = status;
        cached_status["rate_limits"]["five_hour"]["used_percentage"] = json!(50);
        store.ingest_claude_statusline(cached_status, 1400).unwrap();
        drop(store);
        let reopened = Store::open(&path).unwrap();
        let snapshot = reopened.claude_snapshot().unwrap().unwrap();
        assert_eq!(snapshot["observedAt"], 1000);
        assert_eq!(snapshot["observedTokens"], 700);
        assert_eq!(
            snapshot["dailyUsage"],
            json!([{"date":"1970-01-01","tokens":700}])
        );
        assert_eq!(snapshot["windows"][0]["remainingPercent"], 75.0);
        assert!(!snapshot.to_string().contains("private content"));
    }

    #[test]
    fn restart_preserves_latest_account_without_combining_accounts() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("history.sqlite");
        let first = json!({"accountId":"one","observedAt":100,"windows":[],"credits":[],"dailyUsage":[{"date":"2026-10-01","tokens":123}]});
        let second =
            json!({"accountId":"two","observedAt":200,"windows":[],"credits":[],"dailyUsage":[]});
        {
            let mut store = Store::open(&path).unwrap();
            store.save(&first).unwrap();
            store.save(&first).unwrap();
            store.save(&second).unwrap();
        }
        let reopened = Store::open(&path).unwrap();
        assert_eq!(reopened.latest("codex").unwrap(), Some(second));
        assert_eq!(reopened.observation_count("one").unwrap(), 1);
        assert_eq!(reopened.observation_count("two").unwrap(), 1);
    }

    #[test]
    fn retained_days_survive_partial_reports_without_recounting_repeated_imports() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("history.sqlite")).unwrap();
        let first = json!({"provider":"openrouter","accountId":"one","observedAt":100,"dailyUsage":[{"date":"2026-10-01","tokens":100},{"date":"2026-10-02","tokens":20}]});
        store.save(&first).unwrap();
        let mut later = json!({"provider":"openrouter","accountId":"one","observedAt":200,"dailyUsage":[{"date":"2026-10-02","tokens":25}]});
        store.save(&later).unwrap();
        store.save(&later).unwrap();
        store.attach_history(&mut later).unwrap();
        assert_eq!(later["importedTokens"], 125);
        assert_eq!(later["historySince"], "2026-10-01");
        assert_eq!(later["dailyUsage"].as_array().unwrap().len(), 2);
        let mut other = json!({"provider":"openrouter","accountId":"two"});
        store.attach_history(&mut other).unwrap();
        assert!(other["dailyUsage"].as_array().unwrap().is_empty());
        assert!(
            other["importedTokens"].is_null(),
            "Missing history is not a measured zero"
        );
    }

    #[test]
    fn newest_other_provider_does_not_replace_the_saved_codex_account() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("history.sqlite")).unwrap();
        let codex = json!({"accountId":"one","observedAt":100,"windows":[],"credits":[]});
        let grok = json!({"provider":"grok","accountId":"grok-two","observedAt":200});
        store.save(&codex).unwrap();
        store.save(&grok).unwrap();
        assert_eq!(store.latest("codex").unwrap(), Some(codex));
        assert_eq!(store.latest("grok").unwrap(), Some(grok));
    }

    #[test]
    fn forecasts_use_saved_resources_only_from_the_current_account() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("forecasts.sqlite")).unwrap();
        let mut snapshot = json!({"provider":"openrouter","accountId":"one","observedAt":1000,"totalCreditsUsd":10.,"totalUsageUsd":1.,"balanceUsd":9.});
        for (at, used) in [(1000, 1.), (1600, 2.), (2200, 3.)] {
            snapshot["observedAt"] = json!(at);
            snapshot["totalUsageUsd"] = json!(used);
            snapshot["balanceUsd"] = json!(10. - used);
            store.save(&snapshot).unwrap();
        }
        let mut other = snapshot.clone();
        other["accountId"] = json!("two");
        other["totalUsageUsd"] = json!(99.);
        store.save(&other).unwrap();
        assert_eq!(store.forecasts(&snapshot, 2200).unwrap()[0]["perHour"], 6.);
        assert!(store.forecasts(&other, 2200).unwrap()[0]["perHour"].is_null());
    }
}
