#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionKind {
    Terminal,
    Agent,
}

impl SessionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Agent => "agent",
        }
    }

    /// The plugin a session of this kind is shown with.
    pub fn plugin_name(self) -> &'static str {
        match self {
            Self::Terminal => "terminal",
            Self::Agent => "agent",
        }
    }

    /// A pane's plugin for a session of this kind.
    pub fn plugin(self) -> super::PluginPane {
        super::PluginPane {
            name: self.plugin_name().to_string(),
            title: match self {
                Self::Terminal => "Terminal",
                Self::Agent => "AI Agent",
            }
            .to_string(),
            state: serde_json::Value::Null,
        }
    }
}
