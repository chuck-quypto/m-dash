use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Codex,
    Claude,
    Grok,
    Openrouter,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Providers {
    pub codex: bool,
    pub claude: bool,
    pub grok: bool,
    pub openrouter: bool,
}

impl Providers {
    pub fn enabled(&self, provider: Provider) -> bool {
        match provider {
            Provider::Codex => self.codex,
            Provider::Claude => self.claude,
            Provider::Grok => self.grok,
            Provider::Openrouter => self.openrouter,
        }
    }

    pub fn any(&self) -> bool {
        self.codex || self.claude || self.grok || self.openrouter
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CompactWindow {
    CodexFiveHour,
    CodexWeekly,
    ClaudeFiveHour,
    ClaudeWeekly,
    GrokAllowance,
}

impl CompactWindow {
    pub fn provider(self) -> Provider {
        match self {
            Self::CodexFiveHour | Self::CodexWeekly => Provider::Codex,
            Self::ClaudeFiveHour | Self::ClaudeWeekly => Provider::Claude,
            Self::GrokAllowance => Provider::Grok,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub providers: Providers,
    /// None discovers available windows; Some preserves the user's chosen windows.
    pub compact_windows: Option<Vec<CompactWindow>>,
    /// Reads accounting fields from Codex and Claude Code session logs on this computer.
    #[serde(default)]
    pub local_logs: bool,
}

impl Settings {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(windows) = &self.compact_windows {
            if windows.len() > 4 {
                return Err("Choose at most four compact allowance windows.".into());
            }
            if windows
                .iter()
                .enumerate()
                .any(|(index, window)| windows[..index].contains(window))
            {
                return Err("Each compact allowance window can appear only once.".into());
            }
        }
        Ok(())
    }

    pub fn migrated(providers: Providers) -> Self {
        Self {
            providers,
            compact_windows: Some(vec![
                CompactWindow::CodexWeekly,
                CompactWindow::ClaudeWeekly,
                CompactWindow::ClaudeFiveHour,
                CompactWindow::GrokAllowance,
            ]),
            local_logs: false,
        }
    }
}
