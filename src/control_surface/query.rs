use crate::workspace::{PaneKind, SessionKind};

pub(super) fn palette_matches(query: &str, fields: &[&str]) -> bool {
    query.is_empty()
        || fields
            .iter()
            .any(|field| normalize_palette_query(field).contains(query))
}

pub(super) fn normalize_palette_query(value: &str) -> String {
    value.trim().to_ascii_lowercase()
}

pub(super) fn session_kind_label(kind: SessionKind) -> &'static str {
    match kind {
        SessionKind::Terminal => "terminal",
        SessionKind::Agent => "agent",
    }
}

pub(super) fn pane_kind_label(kind: PaneKind) -> &'static str {
    match kind {
        PaneKind::Terminal => "terminal",
        PaneKind::Agent => "agent",
    }
}
