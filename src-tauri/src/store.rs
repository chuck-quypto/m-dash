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
            if let (Some(request), Some(tokens)) = (event.request, event.tokens) {
                transaction
                    .execute(
                        "INSERT OR IGNORE INTO claude_requests VALUES (?1,?2,?3,?4)",
                        params![event.account, request, event.occurred_at, tokens],
                    )
                    .map_err(|_| "Cannot save Claude token event.")?;
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
    /// 30 days, and folds Claude request identifiers older than 35 whole UTC days into daily totals.
    pub fn prune(&mut self, now: u64) -> Result<(), String> {
        let horizon = now.saturating_sub(30 * DAY) as i64;
        let request_cutoff = (now.saturating_sub(35 * DAY) / DAY * DAY) as i64;
        let transaction = self
            .0
            .transaction()
            .map_err(|_| "Cannot begin pruning usage history.")?;
        transaction.execute("INSERT INTO daily_tokens SELECT account,date(occurred_at,'unixepoch'),SUM(tokens) FROM claude_requests WHERE occurred_at<?1 GROUP BY 1,2
            ON CONFLICT(account,day) DO UPDATE SET tokens=excluded.tokens", [request_cutoff]).map_err(|_| "Cannot retain Claude daily totals.")?;
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
            CREATE INDEX IF NOT EXISTS claude_requests_time ON claude_requests(account,occurred_at);")
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
