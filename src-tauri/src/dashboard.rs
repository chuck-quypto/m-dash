use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Dashboard {
    pub settings: crate::settings::Settings,
    pub needs_setup: bool,
    pub snapshot: Option<Value>,
    pub error: Option<String>,
    pub refreshing: bool,
    pub last_attempt: Option<u64>,
    pub next_refresh: Option<u64>,
    pub observations: i64,
    pub grok: Option<Value>,
    pub grok_error: Option<String>,
    pub claude: Option<Value>,
    pub claude_error: Option<String>,
    pub claude_usage_error: Option<String>,
    pub claude_enabled: bool,
    pub claude_ping: Value,
    pub openrouter: Option<Value>,
    pub openrouter_error: Option<String>,
    pub openrouter_configured: bool,
    pub forecasts: Vec<Value>,
    /// Burn ledgers keyed by provider, comparing allowance use with observed tokens.
    pub consistency: Value,
    pub usage_relay_error: Option<String>,
}

impl Dashboard {
    pub fn claude_failed(&self) -> bool {
        if self
            .claude
            .as_ref()
            .is_some_and(|s| s["source"] == "Claude Code account usage")
        {
            self.claude_usage_error.is_some()
        } else {
            !self.claude_enabled || self.claude_error.is_some()
        }
    }
    pub fn apply_result(&mut self, result: Result<Value, String>) {
        match result {
            Ok(snapshot) => {
                self.snapshot = Some(snapshot);
                self.error = None;
            }
            Err(message) => self.error = Some(message),
        }
    }

    pub fn apply_grok_result(&mut self, result: Result<Value, String>) {
        match result {
            Ok(snapshot) => {
                self.grok = Some(snapshot);
                self.grok_error = None;
            }
            Err(message) => self.grok_error = Some(message),
        }
    }
}

#[derive(Default)]
pub struct Schedule {
    pub next: u64,
    failures: u32,
}

impl Schedule {
    pub fn due(&self, now: u64, force: bool) -> bool {
        force || self.next <= now
    }

    pub fn finish(&mut self, now: u64, failed: bool, visible: bool) {
        self.failures = if failed {
            (self.failures + 1).min(4)
        } else {
            0
        };
        let interval = if self.failures > 0 {
            (60_u64 * 2_u64.pow(self.failures)).min(900)
        } else if visible {
            60
        } else {
            300
        };
        self.next = now + interval;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn refresh_failure_preserves_the_last_observation_and_surfaces_the_failure() {
        let old = json!({"observedAt":100,"accountId":"one"});
        let mut dashboard = Dashboard {
            snapshot: Some(old.clone()),
            refreshing: true,
            ..Default::default()
        };
        dashboard.apply_result(Err("Connection lost".into()));
        assert_eq!(dashboard.snapshot, Some(old));
        assert_eq!(dashboard.error.as_deref(), Some("Connection lost"));
        assert!(dashboard.refreshing);
    }

    #[test]
    fn grok_failure_keeps_its_last_values_without_changing_codex() {
        let codex = json!({"observedAt":200,"accountId":"codex-one"});
        let grok = json!({"observedAt":100,"accountId":"grok-one"});
        let mut dashboard = Dashboard {
            snapshot: Some(codex.clone()),
            grok: Some(grok.clone()),
            ..Default::default()
        };
        dashboard.apply_grok_result(Err("Run grok login, then refresh.".into()));
        assert_eq!(dashboard.snapshot, Some(codex));
        assert_eq!(dashboard.grok, Some(grok));
        assert!(dashboard.error.is_none());
        assert_eq!(
            dashboard.grok_error.as_deref(),
            Some("Run grok login, then refresh.")
        );
        dashboard.apply_grok_result(Ok(json!({"accountId":"grok-two","observedAt":300})));
        assert!(dashboard.grok_error.is_none());
        assert_eq!(dashboard.grok.as_ref().unwrap()["accountId"], "grok-two");
    }

    #[test]
    fn failed_provider_backs_off_independently_and_manual_refresh_can_retry() {
        let mut codex = Schedule::default();
        let mut grok = Schedule::default();
        codex.finish(100, false, true);
        grok.finish(100, true, true);
        assert!(codex.due(160, false));
        assert!(!grok.due(160, false));
        assert!(grok.due(160, true));
        grok.finish(160, false, false);
        assert_eq!(grok.next, 460);
        assert_eq!(grok.failures, 0);
    }
}
