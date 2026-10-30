use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::path::Path;

const DAY: u64 = 86_400;

pub struct Store(Connection);
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
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO claude_requests VALUES (?1,?2,?3,?4)",
                        params![event.account, request, event.occurred_at, tokens],
                    )
                    .map_err(|_| "Cannot save Claude token event.")?;
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
        let account: Option<String> = self.0.query_row("SELECT account FROM (SELECT account,updated_at,0 AS priority FROM claude_sessions WHERE account IS NOT NULL UNION ALL SELECT account,updated_at,1 AS priority FROM claude_account_usage UNION ALL SELECT value AS account,0 AS updated_at,2 AS priority FROM settings WHERE key='claude-current-account') ORDER BY priority DESC,updated_at DESC LIMIT 1",[],
            |row| row.get(0)).optional().map_err(|_| "Cannot read Claude account identity.")?;
        let Some(account) = account else {
            return Ok(None);
        };
        let quota: Option<(String,i64,String,Option<String>)> = self.0.query_row("SELECT quota,quota_at,source,(SELECT plan FROM claude_account_usage WHERE account=?1) FROM (SELECT quota,quota_at,'Claude Code statusline' AS source FROM claude_sessions WHERE account=?1 AND quota IS NOT NULL UNION ALL SELECT quota,quota_at,'Claude Code account usage' AS source FROM claude_account_usage WHERE account=?1) ORDER BY quota_at DESC LIMIT 1",[&account],
            |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional().map_err(|_| "Cannot read Claude quota history.")?;
        // Days whose request detail was pruned survive only as retained daily totals.
        let retained = "SELECT day,tokens FROM daily_tokens WHERE account=?1 AND day NOT IN (SELECT date(occurred_at,'unixepoch') FROM claude_requests WHERE account=?1)";
        let (since,last): (Option<i64>,Option<i64>) = self.0.query_row(&format!("SELECT (SELECT MIN(start) FROM (SELECT MIN(occurred_at) AS start FROM claude_requests WHERE account=?1
            UNION ALL SELECT CAST(strftime('%s',day) AS INTEGER) FROM ({retained}))),(SELECT MAX(occurred_at) FROM claude_requests WHERE account=?1)"),[&account],
            |row| Ok((row.get(0)?,row.get(1)?))).map_err(|_| "Cannot read Claude token totals.")?;
        let mut statement = self.0.prepare(&format!("SELECT date(occurred_at,'unixepoch'),SUM(tokens) FROM claude_requests WHERE account=?1 GROUP BY 1 UNION ALL {retained} ORDER BY 1"))
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
        transaction.execute("INSERT INTO daily_tokens SELECT account,date(occurred_at,'unixepoch'),SUM(tokens) FROM claude_requests WHERE occurred_at<?1 GROUP BY 1,2
            ON CONFLICT(account,day) DO UPDATE SET tokens=excluded.tokens", [request_cutoff]).map_err(|_| "Cannot retain Claude daily totals.")?;
        transaction.execute("INSERT INTO usage_daily SELECT account,date(occurred_at,'unixepoch'),surface,COALESCE(model,''),COALESCE(query_source,''),
            COUNT(*),SUM(input),SUM(output),SUM(cache_read),SUM(cache_write),SUM(reasoning),SUM(cost_micros) FROM usage_events WHERE occurred_at<?1 GROUP BY 1,2,3,4,5
            ON CONFLICT(account,day,surface,model,query_source) DO UPDATE SET requests=excluded.requests,input=excluded.input,output=excluded.output,
            cache_read=excluded.cache_read,cache_write=excluded.cache_write,reasoning=excluded.reasoning,cost_micros=excluded.cost_micros", [usage_cutoff])
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
        let db = Connection::open(path).map_err(|_| "Cannot open local usage history.")?;
        db.execute_batch("PRAGMA journal_mode=WAL;
            CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY, value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS latest (account TEXT PRIMARY KEY, observed_at INTEGER NOT NULL, snapshot TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS observations (account TEXT NOT NULL, observed_at INTEGER NOT NULL, resources TEXT NOT NULL, PRIMARY KEY(account, observed_at));
            CREATE TABLE IF NOT EXISTS daily_tokens (account TEXT NOT NULL, day TEXT NOT NULL, tokens INTEGER NOT NULL, PRIMARY KEY(account, day));
            CREATE TABLE IF NOT EXISTS claude_sessions (session TEXT PRIMARY KEY, account TEXT, quota TEXT, api_duration_ms INTEGER, quota_at INTEGER, updated_at INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS claude_requests (account TEXT NOT NULL, request TEXT NOT NULL, occurred_at INTEGER NOT NULL, tokens INTEGER NOT NULL, PRIMARY KEY(account,request));
            CREATE TABLE IF NOT EXISTS claude_account_usage (account TEXT PRIMARY KEY, quota TEXT NOT NULL, quota_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, plan TEXT);
            CREATE INDEX IF NOT EXISTS claude_requests_time ON claude_requests(account,occurred_at);
            CREATE TABLE IF NOT EXISTS usage_events (account TEXT NOT NULL, source TEXT NOT NULL, key TEXT NOT NULL, occurred_at INTEGER NOT NULL,
                surface TEXT NOT NULL, model TEXT, effort TEXT, speed TEXT, query_source TEXT, agent TEXT, input INTEGER NOT NULL, output INTEGER NOT NULL,
                cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL, reasoning INTEGER, cost_micros INTEGER, PRIMARY KEY(account,key));
            CREATE INDEX IF NOT EXISTS usage_events_time ON usage_events(account,occurred_at);
            CREATE TABLE IF NOT EXISTS usage_daily (account TEXT NOT NULL, day TEXT NOT NULL, surface TEXT NOT NULL, model TEXT NOT NULL, query_source TEXT NOT NULL,
                requests INTEGER NOT NULL, input INTEGER NOT NULL, output INTEGER NOT NULL, cache_read INTEGER NOT NULL, cache_write INTEGER NOT NULL,
                reasoning INTEGER, cost_micros INTEGER, PRIMARY KEY(account,day,surface,model,query_source));
            CREATE TABLE IF NOT EXISTS limit_readings (account TEXT NOT NULL, window TEXT NOT NULL, observed_at INTEGER NOT NULL, used_percent REAL NOT NULL,
                resets_at INTEGER, PRIMARY KEY(account,window,observed_at));
            CREATE TABLE IF NOT EXISTS limit_events (account TEXT NOT NULL, occurred_at INTEGER NOT NULL, kind TEXT NOT NULL, PRIMARY KEY(account,occurred_at,kind));
            CREATE TABLE IF NOT EXISTS log_cursors (provider TEXT NOT NULL, path TEXT NOT NULL, offset INTEGER NOT NULL, state TEXT NOT NULL, PRIMARY KEY(provider,path));")
            .map_err(|_| "Cannot initialize local usage history.")?;
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
        for (window, used, resets_at) in crate::usage::windows(snapshot) {
            transaction
                .execute(
                    "INSERT OR IGNORE INTO limit_readings VALUES (?1,?2,?3,?4,?5)",
                    params![account, window, time, used, resets_at],
                )
                .map_err(|_| "Cannot save allowance reading.")?;
        }
        if let Some(days) = snapshot["dailyUsage"].as_array() {
            for day in days {
                let date = day["date"]
                    .as_str()
                    .ok_or("Daily usage is missing its date.")?;
                let tokens = day["tokens"]
                    .as_i64()
                    .filter(|value| *value >= 0)
                    .ok_or("Daily usage is missing its token total.")?;
                transaction.execute("INSERT INTO daily_tokens VALUES (?1,?2,?3) ON CONFLICT(account,day) DO UPDATE SET tokens=excluded.tokens",
                    params![account,date,tokens]).map_err(|_| "Cannot save daily token history.")?;
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
        let mut query = self
            .0
            .prepare("SELECT path,offset,state FROM log_cursors WHERE provider=?1")
            .map_err(|_| "Cannot read local log positions.")?;
        let rows = query
            .query_map([provider], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .map_err(|_| "Cannot read local log positions.")?;
        let mut cursors = std::collections::HashMap::new();
        for row in rows {
            let (path, offset, state) = row.map_err(|_| "Cannot read local log positions.")?;
            cursors.insert(
                path,
                crate::local_logs::Cursor {
                    offset: offset.max(0) as u64,
                    state: serde_json::from_str(&state).unwrap_or(Value::Null),
                },
            );
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
            transaction.execute("INSERT INTO log_cursors VALUES (?1,?2,?3,?4) ON CONFLICT(provider,path) DO UPDATE SET offset=excluded.offset,state=excluded.state",
                params![provider, path, cursor.offset as i64, cursor.state.to_string()]).map_err(|_| "Cannot save local log positions.")?;
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
        let account = snapshot["accountId"]
            .as_str()
            .ok_or("Missing ledger account.")?;
        let since = now.saturating_sub(days * DAY) as i64;
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
        let mut query = self.0.prepare("SELECT window,observed_at,used_percent,resets_at FROM limit_readings WHERE account=?1 AND observed_at>=?2 ORDER BY observed_at").map_err(error)?;
        let readings = query
            .query_map(params![account, since], |row| {
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
            .filter(|reading| labels.contains_key(&reading.window))
            .collect::<Vec<_>>();
        let mut query = self.0.prepare("SELECT occurred_at,surface,COALESCE(model,''),COALESCE(query_source,''),input,output,cache_read,cache_write,cost_micros FROM usage_events WHERE account=?1 AND occurred_at>=?2 ORDER BY occurred_at").map_err(error)?;
        let usage = query
            .query_map(params![account, since], |row| {
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
        let mut result = crate::burn::analyze(provider, &labels, &readings, &usage, &server);
        result["account"] = json!(account);
        result["days"] = json!(days);
        result["limitHits"] = json!(limit_hits);
        Ok(result)
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

    /// Rows recorded on this computer that the usage channel has not received yet, with the
    /// position to mark once they are sent. Rows received from the channel are never re-sent,
    /// and rows older than the relay's 14-day retention are not sent at all.
    pub fn unshared_usage(
        &self,
        channel: &str,
        limit: i64,
    ) -> Result<(i64, Vec<crate::usage::Keyed>), String> {
        let after = self.relay_state(channel)?["pushed"].as_i64().unwrap_or(0);
        let error = |_| "Cannot read usage for the usage channel.".to_string();
        let mut query = self.0.prepare("SELECT rowid,account,key,occurred_at,surface,model,effort,speed,query_source,agent,input,output,cache_read,cache_write,reasoning,cost_micros
            FROM usage_events WHERE rowid>?1 AND source!='relay' AND occurred_at>=CAST(strftime('%s','now') AS INTEGER)-1209600 ORDER BY rowid LIMIT ?2").map_err(error)?;
        let mut last = after;
        let rows = query
            .query_map(params![after, limit], |row| {
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
            .map_err(error)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(error)?
            .into_iter()
            .map(|(rowid, account, key, at, request)| {
                last = last.max(rowid);
                (account, key, at, request)
            })
            .collect();
        Ok((last, rows))
    }

    pub fn mark_shared(&mut self, channel: &str, through: i64) -> Result<(), String> {
        let mut state = self.relay_state(channel)?;
        state["pushed"] = json!(through);
        Self::save_relay_state(&self.0, channel, &state)
    }

    pub fn relay_cursor(&self, channel: &str) -> Result<i64, String> {
        Ok(self.relay_state(channel)?["seq"].as_i64().unwrap_or(0))
    }

    /// Stores rows read from the usage channel together with the read position.
    pub fn ingest_relay(
        &mut self,
        channel: &str,
        rows: &[(i64, crate::usage::Keyed)],
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
        "INSERT INTO usage_events VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16)
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
            request.cost_micros
        ],
    )
    .map_err(|_| "Cannot save usage event.")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
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
        let (through, rows) = store.unshared_usage("chan", 2).unwrap();
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
                9,
            )
            .unwrap();
        assert_eq!(store.relay_cursor("chan").unwrap(), 9);
        let (_, rest) = store.unshared_usage("chan", 10).unwrap();
        assert_eq!(
            rest.iter().map(|row| row.1.as_str()).collect::<Vec<_>>(),
            ["c"]
        );
        assert_eq!(store.unshared_usage("other", 10).unwrap().1.len(), 3);
        assert_eq!(store.relay_cursor("other").unwrap(), 0);
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
    fn pruning_keeps_claude_daily_totals_and_drops_only_old_detail() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = Store::open(&directory.path().join("prune.sqlite")).unwrap();
        let event = |request: &str, at: &str, session: &str| {
            json!({"resourceLogs":[{"resource":{"attributes":[{"key":"service.name","value":{"stringValue":"claude-code"}}]},
            "scopeLogs":[{"logRecords":[{"body":{"stringValue":"claude_code.api_request"},"attributes":[
            {"key":"user.account_uuid","value":{"stringValue":"one"}}, {"key":"organization.id","value":{"stringValue":"org"}},
            {"key":"session.id","value":{"stringValue":session}}, {"key":"request_id","value":{"stringValue":request}},
            {"key":"event.timestamp","value":{"stringValue":at}}, {"key":"input_tokens","value":{"intValue":"10"}},
            {"key":"output_tokens","value":{"intValue":"5"}},
            {"key":"cache_read_tokens","value":{"intValue":"0"}}, {"key":"cache_creation_tokens","value":{"intValue":"0"}}]}]}]}]})
        };
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
