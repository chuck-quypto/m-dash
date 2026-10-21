use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const MODEL: &str = "claude-haiku-5-5";

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct State {
    pub enabled_account: Option<String>,
    records: BTreeMap<String, Record>,
    consented_model: Option<String>,
    pub message: String,
}

#[derive(Clone, Default, Deserialize, Serialize)]
struct Record {
    reset: u64,
    attempted_reset: Option<u64>,
    attempted_at: Option<u64>,
    tokens: Option<u64>,
    outcome: Option<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Attempt {
    pub account: String,
    pub reset: u64,
    pub at: u64,
}

impl State {
    pub fn defer(&mut self, message: &str) {
        self.message = format!("Paused until the next fresh allowance read: {message}");
    }
    pub fn configure(&mut self, account: Option<&str>, confirmed: bool) -> Result<(), String> {
        if account.is_some() && !confirmed {
            return Err("Confirm the usage notice before enabling automatic pings.".into());
        }
        self.enabled_account = account.map(str::to_owned);
        self.consented_model = account.map(|_| MODEL.to_owned());
        self.message = if account.is_some() {
            "Waiting for a fresh Claude allowance read."
        } else {
            "Off. No automatic model requests."
        }
        .into();
        Ok(())
    }

    pub fn observe(&mut self, snapshot: &Value, now: u64) -> Option<Attempt> {
        let account = self.enabled_account.clone()?;
        if self.consented_model.as_deref() != Some(MODEL) {
            self.enabled_account = None;
            self.message =
                "Off: the ping model changed. Review and confirm the usage notice again.".into();
            return None;
        }
        if snapshot["accountId"].as_str() != Some(&account) {
            self.enabled_account = None;
            self.message = "Off: Claude sign-in changed. Confirm again for the new account.".into();
            return None;
        }
        if snapshot["source"] != "Claude Code account usage"
            || snapshot["pingFresh"] != true
            || !snapshot["quotaObservedAt"]
                .as_u64()
                .is_some_and(|at| at > 0 && at <= now && now - at <= 60)
            || !matches!(snapshot["plan"].as_str(), Some("pro" | "max"))
        {
            self.message = "Waiting for a fresh Pro or Max subscription allowance read.".into();
            return None;
        }
        let record = self.records.entry(account.clone()).or_default();
        if let Some(reset) = snapshot["fiveHourReset"].as_u64() {
            record.reset = record.reset.max(reset);
        }
        if snapshot["fiveHourState"] == "active" {
            self.message = "Waiting for the current five-hour window to expire.".into();
            return None;
        }
        if !matches!(snapshot["fiveHourState"].as_str(), Some("idle" | "expired"))
            || record.reset > now
        {
            self.message = "Waiting for an unambiguous idle five-hour window.".into();
            return None;
        }
        let weekly_available = snapshot["windows"].as_array().is_some_and(|windows| {
            windows.iter().any(|w| {
                w["id"] == "seven_day"
                    && w["remainingPercent"]
                        .as_f64()
                        .is_some_and(|n| n > 0.0 && n <= 100.0)
                    && w["resetsAt"].as_u64().is_some_and(|at| at > now)
            })
        });
        if !weekly_available {
            self.message = "Paused: weekly allowance is unavailable or exhausted.".into();
            return None;
        }
        if record.attempted_reset == Some(record.reset)
            || record
                .attempted_at
                .is_some_and(|at| now < at.saturating_add(18000))
        {
            self.message =
                "Already attempted. Waiting for a new five-hour window; no automatic retry.".into();
            return None;
        }
        record.attempted_reset = Some(record.reset);
        record.attempted_at = Some(now);
        record.tokens = None;
        record.outcome = Some("pending-or-interrupted".into());
        self.message = "Ping attempt recorded. An interrupted attempt will not be retried.".into();
        Some(Attempt {
            account,
            reset: record.reset,
            at: now,
        })
    }

    pub fn finish(&mut self, attempt: &Attempt, result: Result<Option<u64>, String>) {
        if let Some(record) = self.records.get_mut(&attempt.account) {
            if record.attempted_at == Some(attempt.at)
                && record.attempted_reset == Some(attempt.reset)
            {
                record.outcome = Some(
                    if result.is_ok() {
                        "completed"
                    } else {
                        "failed-or-unknown"
                    }
                    .into(),
                );
                if let Ok(tokens) = &result {
                    record.tokens = *tokens;
                }
                if self.enabled_account.as_deref() == Some(&attempt.account) {
                    self.message = match result {
                        Ok(_) => {
                            "Ping completed. The next allowance read will check the window.".into()
                        }
                        Err(message) => message,
                    };
                }
            }
        }
    }

    pub fn status(&self) -> Value {
        let record = self
            .enabled_account
            .as_ref()
            .and_then(|id| self.records.get(id));
        serde_json::json!({"enabled":self.enabled_account.is_some(),"message":if self.message.is_empty() { "Off. No automatic model requests." } else { &self.message },
            "lastAttempt":record.and_then(|r| r.attempted_at),"lastTokens":record.and_then(|r| r.tokens),"lastOutcome":record.and_then(|r| r.outcome.as_deref())})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn idle(now: u64) -> Value {
        json!({"accountId":"claude:one","source":"Claude Code account usage","plan":"pro",
            "quotaObservedAt":now,"pingFresh":true,"fiveHourState":"idle","fiveHourReset":null,
            "windows":[{"id":"seven_day","remainingPercent":80,"resetsAt":now+604800}]})
    }

    #[test]
    fn transient_preflight_failure_preserves_consent_without_recording_a_model_attempt() {
        let mut state = State::default();
        state.configure(Some("claude:one"), true).unwrap();
        state.defer("CLI timed out");
        assert_eq!(state.status()["enabled"], true);
        assert!(state.status()["lastAttempt"].is_null());
        assert!(state.observe(&idle(1000), 1000).is_some());
    }

    #[test]
    fn uncertain_stale_blocked_or_other_account_readings_cannot_send() {
        for change in [
            json!({"pingFresh":false}),
            json!({"quotaObservedAt":900}),
            json!({"quotaObservedAt":1001}),
            json!({"fiveHourState":"unknown"}),
            json!({"fiveHourState":"active","fiveHourReset":1200}),
            json!({"plan":"team"}),
            json!({"source":"Claude Code statusline"}),
            json!({"accountId":"claude:other"}),
            json!({"windows":[]}),
            json!({"windows":[{"id":"seven_day","remainingPercent":0,"resetsAt":9999}]}),
            json!({"windows":[{"id":"seven_day","remainingPercent":90,"resetsAt":999}]}),
        ] {
            let mut state = State::default();
            state.configure(Some("claude:one"), true).unwrap();
            let mut snapshot = idle(1000);
            for (key, value) in change.as_object().unwrap() {
                snapshot[key] = value.clone();
            }
            assert!(state.observe(&snapshot, 1000).is_none(), "{change}");
        }
    }

    #[test]
    fn only_a_new_observed_window_allows_another_attempt_after_failure() {
        let mut state = State::default();
        state.configure(Some("claude:one"), true).unwrap();
        let attempt = state.observe(&idle(1000), 1000).unwrap();
        state.finish(&attempt, Err("Outcome unknown; no retry.".into()));
        assert!(state.observe(&idle(2000), 2000).is_none());
        let mut active = idle(2000);
        active["fiveHourState"] = json!("active");
        active["fiveHourReset"] = json!(20000);
        assert!(state.observe(&active, 2000).is_none());
        assert!(state.observe(&idle(19999), 19999).is_none());
        assert_eq!(state.observe(&idle(20000), 20000).unwrap().reset, 20000);
        state.configure(None, false).unwrap();
        assert!(state.observe(&idle(40000), 40000).is_none());
    }

    #[test]
    fn pings_require_explicit_consent_and_one_attempt_survives_restart() {
        let mut state = State::default();
        assert!(state.observe(&idle(1000), 1000).is_none());
        assert!(state.configure(Some("claude:one"), false).is_err());
        state.configure(Some("claude:one"), true).unwrap();
        assert_eq!(
            state.observe(&idle(1000), 1000),
            Some(Attempt {
                account: "claude:one".into(),
                reset: 0,
                at: 1000
            })
        );
        let mut restarted: State =
            serde_json::from_str(&serde_json::to_string(&state).unwrap()).unwrap();
        assert!(
            restarted.observe(&idle(20000), 20000).is_none(),
            "no retry without a new window even after five hours"
        );
        restarted.configure(None, false).unwrap();
        restarted.configure(Some("claude:one"), true).unwrap();
        assert!(
            restarted.observe(&idle(30000), 30000).is_none(),
            "toggle cannot reset attempt markers"
        );
    }
}
