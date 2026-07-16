use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::types::{
    LayoutChild, LayoutNode, Pane, PaneId, PaneKind, SessionKind, SplitAxis, Tab, TabId, ViewKind,
    Workspace, WorkspaceSession,
};
use crate::SessionId;

pub const WORKSPACE_STATE_VERSION: u32 = 1;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionInventory {
    pub terminal: Vec<SessionId>,
    pub agent: Vec<SessionId>,
}

impl SessionInventory {
    pub fn new(terminal: Vec<SessionId>, agent: Vec<SessionId>) -> Self {
        Self { terminal, agent }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventoryReconcile {
    pub removed: Vec<SessionId>,
    pub added: Vec<SessionId>,
    /// A fresh terminal created when pruning removed every persisted pane.
    /// The caller must create this session in the daemon.
    pub created_terminal: Option<SessionId>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum WorkspaceStateError {
    Malformed(String),
    UnsupportedVersion { found: u32, supported: u32 },
    Invalid(String),
}

impl fmt::Display for WorkspaceStateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Malformed(message) => write!(formatter, "malformed workspace state: {message}"),
            Self::UnsupportedVersion { found, supported } => write!(
                formatter,
                "unsupported workspace state version {found}; expected {supported}"
            ),
            Self::Invalid(message) => write!(formatter, "invalid workspace state: {message}"),
        }
    }
}

impl Error for WorkspaceStateError {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventoryError(String);

impl fmt::Display for InventoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for InventoryError {}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct WorkspaceState {
    version: u32,
    active_tab: TabId,
    next_terminal_display_number: usize,
    next_agent_display_number: usize,
    tabs: Vec<TabState>,
    sessions: Vec<SessionState>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct TabState {
    id: TabId,
    active_pane: PaneId,
    root: LayoutState,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum LayoutState {
    Pane {
        pane: PaneState,
    },
    Split {
        axis: AxisState,
        children: Vec<LayoutChildState>,
    },
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct LayoutChildState {
    weight: f32,
    node: LayoutState,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PaneState {
    id: PaneId,
    kind: PaneKindState,
    session_id: Option<SessionId>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionState {
    id: SessionId,
    kind: SessionKindState,
    display_number: usize,
    title: String,
}

/// A session's persisted kind -- always session-backed
/// (`Terminal`/`Agent`), unlike [`PaneKindState`], since a `PaneKind::View`
/// pane never has a session at all (see `WorkspaceState::validate`'s pane
/// loop, which checks a view pane has no `session_id` rather than looking
/// one up here).
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum SessionKindState {
    Terminal,
    Agent,
}

/// A pane's persisted kind. Serializes as a bare string for the
/// session-backed variants (`"terminal"`/`"agent"`, unchanged from before
/// `PaneKind::View` existed) and as `{"view": "theme_settings"}` for a
/// first-party view pane (serde's default externally-tagged
/// representation for a newtype variant) -- distinct from
/// [`SessionKindState`] because a view pane's kind space
/// ([`ViewKindState`]) has no session-kind counterpart.
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum PaneKindState {
    Terminal,
    Agent,
    View(ViewKindState),
}

/// Persisted counterpart of [`ViewKind`]; add a variant here alongside a
/// new `ViewKind` for every future first-party view.
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum ViewKindState {
    ThemeSettings,
}

impl PaneKindState {
    /// The session kind a pane of this persisted kind must attach --
    /// `None` for `View`, which must instead have no `session_id` at all
    /// (see the pane loop in `WorkspaceState::validate`).
    fn expected_session_kind(self) -> Option<SessionKind> {
        match self {
            Self::Terminal => Some(SessionKind::Terminal),
            Self::Agent => Some(SessionKind::Agent),
            Self::View(_) => None,
        }
    }
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum AxisState {
    Horizontal,
    Vertical,
}

impl Workspace {
    pub fn to_persisted_json(&self) -> Result<String, WorkspaceStateError> {
        let state = WorkspaceState::capture(self)?;
        state.validate()?;
        serde_json::to_string_pretty(&state)
            .map_err(|error| WorkspaceStateError::Invalid(format!("serialize: {error}")))
    }

    pub fn from_persisted_json(json: &str) -> Result<Self, WorkspaceStateError> {
        let state: WorkspaceState = serde_json::from_str(json)
            .map_err(|error| WorkspaceStateError::Malformed(error.to_string()))?;
        state.validate()?;
        Ok(state.into_workspace())
    }

    /// Reconciles restored metadata with the authoritative daemon inventory.
    /// The operation validates the complete inventory before changing the
    /// workspace, so an invalid response leaves the model untouched.
    pub fn reconcile_session_inventory(
        &mut self,
        inventory: &SessionInventory,
    ) -> Result<InventoryReconcile, InventoryError> {
        let inventory_by_id = inventory.validate()?;
        for session in &self.sessions {
            if let Some(kind) = inventory_by_id.get(&session.id) {
                if *kind != session.kind {
                    return Err(InventoryError(format!(
                        "session {:?} changed kind from {} to {}",
                        session.id,
                        session.kind.label(),
                        kind.label()
                    )));
                }
            }
        }

        let mut removed: Vec<_> = self
            .sessions
            .iter()
            .filter(|session| !inventory_by_id.contains_key(&session.id))
            .map(|session| session.id)
            .collect();
        sort_session_ids(&mut removed);
        let removed_set: HashSet<_> = removed.iter().copied().collect();
        self.sessions
            .retain(|session| !removed_set.contains(&session.id));

        let removed_panes: Vec<_> = self
            .panes
            .iter()
            .filter(|pane| pane.session_id.is_some_and(|id| removed_set.contains(&id)))
            .map(|pane| pane.id)
            .collect();
        self.panes.retain(|pane| !removed_panes.contains(&pane.id));
        self.prune_layout_panes(&removed_panes);

        let known: HashSet<_> = self.sessions.iter().map(|session| session.id).collect();
        let mut additions: Vec<_> = inventory_by_id
            .into_iter()
            .filter(|(id, _)| !known.contains(id))
            .collect();
        additions.sort_by_key(|(id, _)| id.as_uuid());
        let mut added = Vec::with_capacity(additions.len());
        for (id, kind) in additions {
            self.register_detached_session(PaneKind::from(kind), id);
            added.push(id);
        }

        let created_terminal = if self.tabs.is_empty() {
            let id = SessionId::new();
            self.open_tab(PaneKind::Terminal, Some(id));
            Some(id)
        } else {
            None
        };

        Ok(InventoryReconcile {
            removed,
            added,
            created_terminal,
        })
    }

    fn prune_layout_panes(&mut self, removed: &[PaneId]) {
        let mut retained_tabs = Vec::with_capacity(self.tabs.len());
        for mut tab in self.tabs.drain(..) {
            let mut root = Some(tab.root);
            for pane_id in removed {
                root = root.and_then(|node| node.without_pane(*pane_id));
            }
            let Some(mut root) = root else {
                continue;
            };
            root.flatten();
            tab.root = root;
            if !tab.root.pane_ids().contains(&tab.active) {
                tab.active = tab
                    .root
                    .first_pane()
                    .expect("a retained layout contains a pane");
            }
            retained_tabs.push(tab);
        }
        self.tabs = retained_tabs;
        if !self.tabs.iter().any(|tab| tab.id == self.active_tab) {
            if let Some(tab) = self.tabs.first() {
                self.active_tab = tab.id;
            }
        }
        self.workspace_mode_cursor = None;
    }
}

impl SessionInventory {
    fn validate(&self) -> Result<HashMap<SessionId, SessionKind>, InventoryError> {
        let mut result = HashMap::new();
        for (ids, kind) in [
            (&self.terminal, SessionKind::Terminal),
            (&self.agent, SessionKind::Agent),
        ] {
            for id in ids {
                if let Some(previous) = result.insert(*id, kind) {
                    return Err(InventoryError(format!(
                        "session {id:?} appears more than once ({} and {})",
                        previous.label(),
                        kind.label()
                    )));
                }
            }
        }
        Ok(result)
    }
}

impl WorkspaceState {
    fn capture(workspace: &Workspace) -> Result<Self, WorkspaceStateError> {
        let tabs: Vec<_> = workspace
            .tabs
            .iter()
            .map(|tab| {
                Ok(TabState {
                    id: tab.id,
                    active_pane: tab.active,
                    root: LayoutState::capture(&tab.root, &workspace.panes)?,
                })
            })
            .collect::<Result<_, WorkspaceStateError>>()?;
        let referenced_panes: HashSet<_> = workspace
            .tabs
            .iter()
            .flat_map(|tab| tab.root.pane_ids())
            .collect();
        if referenced_panes.len() != workspace.panes.len()
            || workspace
                .panes
                .iter()
                .any(|pane| !referenced_panes.contains(&pane.id))
        {
            return Err(state_error(
                "workspace panes must each appear exactly once in a layout",
            ));
        }

        Ok(Self {
            version: WORKSPACE_STATE_VERSION,
            active_tab: workspace.active_tab,
            next_terminal_display_number: workspace.next_terminal_display_number,
            next_agent_display_number: workspace.next_agent_display_number,
            tabs,
            sessions: workspace
                .sessions
                .iter()
                .map(|session| SessionState {
                    id: session.id,
                    kind: SessionKindState::from(session.kind),
                    display_number: session.display_number,
                    title: session.title.clone(),
                })
                .collect(),
        })
    }

    fn validate(&self) -> Result<(), WorkspaceStateError> {
        if self.version != WORKSPACE_STATE_VERSION {
            return Err(WorkspaceStateError::UnsupportedVersion {
                found: self.version,
                supported: WORKSPACE_STATE_VERSION,
            });
        }
        if self.tabs.is_empty() {
            return Err(state_error("workspace must contain at least one tab"));
        }
        if self.next_terminal_display_number == 0 || self.next_agent_display_number == 0 {
            return Err(state_error("display counters must be positive"));
        }

        let mut session_ids = HashSet::new();
        let mut display_numbers = HashSet::new();
        let mut max_terminal = 0;
        let mut max_agent = 0;
        let mut sessions = HashMap::new();
        for session in &self.sessions {
            if !session_ids.insert(session.id) {
                return Err(state_error(format!(
                    "duplicate session id {:?}",
                    session.id
                )));
            }
            if session.display_number == 0 {
                return Err(state_error("session display numbers must be positive"));
            }
            if session.title.trim().is_empty() {
                return Err(state_error("session titles must not be empty"));
            }
            let kind = SessionKind::from(session.kind);
            if !display_numbers.insert((kind.label(), session.display_number)) {
                return Err(state_error(format!(
                    "duplicate {} display number {}",
                    kind.label(),
                    session.display_number
                )));
            }
            match kind {
                SessionKind::Terminal => max_terminal = max_terminal.max(session.display_number),
                SessionKind::Agent => max_agent = max_agent.max(session.display_number),
            }
            sessions.insert(session.id, kind);
        }
        if self.next_terminal_display_number <= max_terminal
            || self.next_agent_display_number <= max_agent
        {
            return Err(state_error(
                "display counter must exceed every allocated number",
            ));
        }

        let mut tab_ids = HashSet::new();
        let mut pane_ids = HashSet::new();
        let mut attached_sessions = HashSet::new();
        for tab in &self.tabs {
            if !tab_ids.insert(tab.id) {
                return Err(state_error(format!("duplicate tab id {:?}", tab.id)));
            }
            let mut tab_panes = Vec::new();
            tab.root.validate(None, &mut tab_panes)?;
            if !tab_panes.iter().any(|pane| pane.id == tab.active_pane) {
                return Err(state_error(format!(
                    "active pane {:?} is not in tab {:?}",
                    tab.active_pane, tab.id
                )));
            }
            for pane in tab_panes {
                if !pane_ids.insert(pane.id) {
                    return Err(state_error(format!("duplicate pane id {:?}", pane.id)));
                }
                match pane.kind.expected_session_kind() {
                    None => {
                        if pane.session_id.is_some() {
                            return Err(state_error(format!(
                                "view pane {:?} must not have a session attachment",
                                pane.id
                            )));
                        }
                    }
                    Some(expected_kind) => {
                        let Some(session_id) = pane.session_id else {
                            return Err(state_error(format!(
                                "pane {:?} has no session attachment",
                                pane.id
                            )));
                        };
                        let Some(session_kind) = sessions.get(&session_id) else {
                            return Err(state_error(format!(
                                "pane {:?} references unknown session {session_id:?}",
                                pane.id
                            )));
                        };
                        if *session_kind != expected_kind {
                            return Err(state_error(format!(
                                "pane {:?} and session {session_id:?} have different kinds",
                                pane.id
                            )));
                        }
                        if !attached_sessions.insert(session_id) {
                            return Err(state_error(format!(
                                "session {session_id:?} is attached to multiple panes"
                            )));
                        }
                    }
                }
            }
        }
        if !tab_ids.contains(&self.active_tab) {
            return Err(state_error(format!(
                "active tab {:?} does not exist",
                self.active_tab
            )));
        }
        Ok(())
    }

    fn into_workspace(self) -> Workspace {
        let mut panes = Vec::new();
        let tabs = self
            .tabs
            .into_iter()
            .map(|tab| Tab {
                id: tab.id,
                active: tab.active_pane,
                root: tab.root.into_layout(&mut panes),
            })
            .collect();
        Workspace {
            tabs,
            panes,
            sessions: self
                .sessions
                .into_iter()
                .map(|session| WorkspaceSession {
                    id: session.id,
                    kind: SessionKind::from(session.kind),
                    display_number: session.display_number,
                    title: session.title,
                })
                .collect(),
            active_tab: self.active_tab,
            next_terminal_display_number: self.next_terminal_display_number,
            next_agent_display_number: self.next_agent_display_number,
            workspace_mode_cursor: None,
        }
    }
}

impl LayoutState {
    fn capture(node: &LayoutNode, panes: &[Pane]) -> Result<Self, WorkspaceStateError> {
        Ok(match node {
            LayoutNode::Pane(id) => {
                let pane = panes
                    .iter()
                    .find(|pane| pane.id == *id)
                    .ok_or_else(|| state_error(format!("layout references unknown pane {id:?}")))?;
                Self::Pane {
                    pane: PaneState {
                        id: pane.id,
                        kind: PaneKindState::from(pane.kind),
                        session_id: pane.session_id,
                    },
                }
            }
            LayoutNode::Split { axis, children } => Self::Split {
                axis: AxisState::from(*axis),
                children: children
                    .iter()
                    .map(|child| {
                        Ok(LayoutChildState {
                            weight: child.weight,
                            node: Self::capture(&child.node, panes)?,
                        })
                    })
                    .collect::<Result<_, WorkspaceStateError>>()?,
            },
        })
    }

    fn validate<'a>(
        &'a self,
        parent_axis: Option<AxisState>,
        panes: &mut Vec<&'a PaneState>,
    ) -> Result<(), WorkspaceStateError> {
        match self {
            Self::Pane { pane } => panes.push(pane),
            Self::Split { axis, children } => {
                if children.len() < 2 {
                    return Err(state_error("split must contain at least two children"));
                }
                if parent_axis.is_some_and(|parent| parent == *axis) {
                    return Err(state_error(
                        "nested splits with the same axis are not canonical",
                    ));
                }
                for child in children {
                    if !child.weight.is_finite() || child.weight <= 0.0 {
                        return Err(state_error("split weights must be finite and positive"));
                    }
                    child.node.validate(Some(*axis), panes)?;
                }
            }
        }
        Ok(())
    }

    fn into_layout(self, panes: &mut Vec<Pane>) -> LayoutNode {
        match self {
            Self::Pane { pane } => {
                let id = pane.id;
                panes.push(Pane {
                    id,
                    kind: PaneKind::from(pane.kind),
                    session_id: pane.session_id,
                });
                LayoutNode::Pane(id)
            }
            Self::Split { axis, children } => LayoutNode::Split {
                axis: SplitAxis::from(axis),
                children: children
                    .into_iter()
                    .map(|child| LayoutChild {
                        node: child.node.into_layout(panes),
                        weight: child.weight,
                    })
                    .collect(),
            },
        }
    }
}

impl From<SessionKind> for SessionKindState {
    fn from(kind: SessionKind) -> Self {
        match kind {
            SessionKind::Terminal => Self::Terminal,
            SessionKind::Agent => Self::Agent,
        }
    }
}

impl From<SessionKindState> for SessionKind {
    fn from(kind: SessionKindState) -> Self {
        match kind {
            SessionKindState::Terminal => Self::Terminal,
            SessionKindState::Agent => Self::Agent,
        }
    }
}

impl From<PaneKind> for PaneKindState {
    fn from(kind: PaneKind) -> Self {
        match kind {
            PaneKind::Terminal => Self::Terminal,
            PaneKind::Agent => Self::Agent,
            PaneKind::View(view_kind) => Self::View(ViewKindState::from(view_kind)),
        }
    }
}

impl From<PaneKindState> for PaneKind {
    fn from(kind: PaneKindState) -> Self {
        match kind {
            PaneKindState::Terminal => Self::Terminal,
            PaneKindState::Agent => Self::Agent,
            PaneKindState::View(view_kind) => Self::View(ViewKind::from(view_kind)),
        }
    }
}

impl From<ViewKind> for ViewKindState {
    fn from(kind: ViewKind) -> Self {
        match kind {
            ViewKind::ThemeSettings => Self::ThemeSettings,
        }
    }
}

impl From<ViewKindState> for ViewKind {
    fn from(kind: ViewKindState) -> Self {
        match kind {
            ViewKindState::ThemeSettings => Self::ThemeSettings,
        }
    }
}

impl From<SplitAxis> for AxisState {
    fn from(axis: SplitAxis) -> Self {
        match axis {
            SplitAxis::Horizontal => Self::Horizontal,
            SplitAxis::Vertical => Self::Vertical,
        }
    }
}

impl From<AxisState> for SplitAxis {
    fn from(axis: AxisState) -> Self {
        match axis {
            AxisState::Horizontal => Self::Horizontal,
            AxisState::Vertical => Self::Vertical,
        }
    }
}

impl PartialEq for AxisState {
    fn eq(&self, other: &Self) -> bool {
        matches!(
            (self, other),
            (Self::Horizontal, Self::Horizontal) | (Self::Vertical, Self::Vertical)
        )
    }
}

fn state_error(message: impl Into<String>) -> WorkspaceStateError {
    WorkspaceStateError::Invalid(message.into())
}

fn sort_session_ids(ids: &mut [SessionId]) {
    ids.sort_by_key(|id| id.as_uuid());
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    #[test]
    fn state_round_trip_preserves_layout_sessions_titles_and_counters() {
        let mut workspace = Workspace::mvp();
        let terminal = workspace.active_session_id().expect("terminal");
        let agent = SessionId::new();
        workspace.split_active(PaneKind::Agent, Some(agent));
        workspace.register_detached_session(PaneKind::Terminal, SessionId::new());
        workspace
            .sessions
            .iter_mut()
            .find(|session| session.id == terminal)
            .expect("session")
            .title = "editor".into();
        workspace.enter_workspace_mode();

        let json = workspace.to_persisted_json().expect("serialize");
        assert_eq!(
            serde_json::from_str::<Value>(&json).expect("json")["version"],
            WORKSPACE_STATE_VERSION
        );
        let restored = Workspace::from_persisted_json(&json).expect("restore");

        assert_eq!(restored.tabs.len(), workspace.tabs.len());
        assert_eq!(restored.panes.len(), workspace.panes.len());
        assert_eq!(restored.sessions, workspace.sessions);
        assert_eq!(restored.active_tab, workspace.active_tab);
        assert_eq!(
            restored.next_terminal_display_number,
            workspace.next_terminal_display_number
        );
        assert_eq!(
            restored.next_agent_display_number,
            workspace.next_agent_display_number
        );
        assert_eq!(restored.workspace_mode_cursor, None);
        assert_eq!(restored.to_persisted_json().expect("serialize again"), json);
    }

    #[test]
    fn state_round_trip_preserves_a_view_pane_without_a_session() {
        let mut workspace = Workspace::mvp();
        let terminal_pane = workspace.visible_pane_id(0).expect("terminal pane");
        let view_pane =
            workspace.split_active_tab_with_view(ViewKind::ThemeSettings, SplitAxis::Horizontal);

        let json = workspace.to_persisted_json().expect("serialize");
        // The persisted shape for a view pane's kind (no session-backed
        // counterpart to reuse): serde's default externally-tagged
        // representation for a newtype variant.
        let value: Value = serde_json::from_str(&json).expect("json");
        assert_eq!(
            value["tabs"][0]["root"]["children"][1]["node"]["pane"]["kind"],
            json!({"view": "theme_settings"})
        );
        assert!(value["tabs"][0]["root"]["children"][1]["node"]["pane"]["session_id"].is_null());

        let restored = Workspace::from_persisted_json(&json).expect("restore");

        assert_eq!(
            restored.pane_kind(view_pane),
            Some(PaneKind::View(ViewKind::ThemeSettings))
        );
        assert_eq!(
            restored
                .panes
                .iter()
                .find(|pane| pane.id == view_pane)
                .expect("view pane")
                .session_id,
            None
        );
        // No session was ever created for the view pane -- only the mvp
        // terminal's session survives the round trip.
        assert_eq!(restored.session_count(), workspace.session_count());
        assert!(restored.all_pane_ids().contains(&terminal_pane));
        assert_eq!(restored.to_persisted_json().expect("serialize again"), json);
    }

    #[test]
    fn state_embeds_panes_in_layout_leaves() {
        let value: Value =
            serde_json::from_str(&Workspace::mvp().to_persisted_json().expect("serialize"))
                .expect("json");
        assert!(value.get("panes").is_none());
        assert_eq!(value["tabs"][0]["root"]["type"], "pane");
        assert!(value["tabs"][0]["root"]["pane"]["id"].is_string());
    }

    #[test]
    fn malformed_and_unsupported_states_are_distinct() {
        assert!(matches!(
            Workspace::from_persisted_json("{"),
            Err(WorkspaceStateError::Malformed(_))
        ));
        let mut value = state_value();
        value["version"] = json!(WORKSPACE_STATE_VERSION + 1);
        match Workspace::from_persisted_json(&value.to_string()) {
            Err(WorkspaceStateError::UnsupportedVersion { found, supported }) => {
                assert_eq!(found, WORKSPACE_STATE_VERSION + 1);
                assert_eq!(supported, WORKSPACE_STATE_VERSION);
            }
            other => panic!("expected unsupported version, got {other:?}"),
        }
    }

    #[test]
    fn validation_rejects_non_positive_weight_and_noncanonical_split() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Terminal, Some(SessionId::new()));
        let mut value: Value =
            serde_json::from_str(&workspace.to_persisted_json().expect("serialize")).expect("json");
        value["tabs"][0]["root"]["children"][0]["weight"] = json!(0.0);
        assert_invalid(value, "weights");

        let mut value: Value =
            serde_json::from_str(&workspace.to_persisted_json().expect("serialize")).expect("json");
        let leaf = value["tabs"][0]["root"]["children"][0]["node"].take();
        value["tabs"][0]["root"]["children"][0]["node"] = json!({
            "type": "split",
            "axis": "horizontal",
            "children": [
                { "weight": 1.0, "node": leaf.clone() },
                { "weight": 1.0, "node": leaf }
            ]
        });
        assert_invalid(value, "same axis");
    }

    #[test]
    fn validation_rejects_duplicate_ids_kind_mismatch_and_stale_counter() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(PaneKind::Agent, Some(SessionId::new()));

        let mut duplicate = json_value(&workspace);
        let first_id = duplicate["sessions"][0]["id"].clone();
        duplicate["sessions"][1]["id"] = first_id;
        assert_invalid(duplicate, "duplicate session id");

        let mut mismatch = json_value(&workspace);
        mismatch["tabs"][0]["root"]["children"][1]["node"]["pane"]["kind"] = json!("terminal");
        assert_invalid(mismatch, "different kinds");

        let mut counter = json_value(&workspace);
        counter["next_terminal_display_number"] = json!(1);
        assert_invalid(counter, "counter");
    }

    #[test]
    fn validation_rejects_a_pane_without_a_session() {
        let mut value = state_value();
        value["tabs"][0]["root"]["pane"]["session_id"] = Value::Null;
        assert_invalid(value, "no session attachment");
    }

    #[test]
    fn validation_rejects_a_view_pane_with_a_session_attachment() {
        let mut workspace = Workspace::mvp();
        workspace.split_active_tab_with_view(ViewKind::ThemeSettings, SplitAxis::Horizontal);
        let mut value = json_value(&workspace);
        // Any session id is rejected here regardless of whether it's
        // known -- a view pane must have none at all.
        let borrowed_session_id = value["sessions"][0]["id"].clone();
        value["tabs"][0]["root"]["children"][1]["node"]["pane"]["session_id"] = borrowed_session_id;
        assert_invalid(value, "must not have a session attachment");
    }

    #[test]
    fn inventory_prunes_missing_panes_collapses_tree_and_repairs_active_state() {
        let mut workspace = Workspace::mvp();
        let retained = workspace.active_session_id().expect("terminal");
        let missing_agent = SessionId::new();
        workspace.split_active(PaneKind::Agent, Some(missing_agent));
        let second_tab = SessionId::new();
        workspace.open_tab(PaneKind::Terminal, Some(second_tab));
        let daemon_only = SessionId::new();

        let outcome = workspace
            .reconcile_session_inventory(&SessionInventory::new(
                vec![retained, daemon_only],
                vec![],
            ))
            .expect("reconcile");

        assert_eq!(outcome.removed.len(), 2);
        assert!(outcome.removed.contains(&missing_agent));
        assert!(outcome.removed.contains(&second_tab));
        assert_eq!(outcome.added, vec![daemon_only]);
        assert_eq!(outcome.created_terminal, None);
        assert_eq!(workspace.tabs.len(), 1);
        assert!(matches!(workspace.tabs[0].root, LayoutNode::Pane(_)));
        assert_eq!(workspace.active_session_id(), Some(retained));
        assert_eq!(workspace.detached_session_count(), 1);
    }

    #[test]
    fn inventory_preserves_detached_metadata_and_creates_terminal_when_all_panes_are_gone() {
        let mut workspace = Workspace::mvp();
        let attached = workspace.active_session_id().expect("terminal");
        let detached = SessionId::new();
        workspace.register_detached_session(PaneKind::Agent, detached);
        let detached_before = workspace.session(detached).expect("detached").clone();

        let outcome = workspace
            .reconcile_session_inventory(&SessionInventory::new(vec![], vec![detached]))
            .expect("reconcile");

        assert_eq!(outcome.removed, vec![attached]);
        let created = outcome.created_terminal.expect("fresh terminal");
        assert_eq!(workspace.active_session_id(), Some(created));
        assert_eq!(workspace.session(detached), Some(&detached_before));
        assert_eq!(workspace.detached_session_count(), 1);
        assert!(workspace.next_terminal_display_number > 1);
    }

    #[test]
    fn invalid_inventory_is_atomic() {
        let mut workspace = Workspace::mvp();
        let before = workspace.to_persisted_json().expect("serialize");
        let id = workspace.active_session_id().expect("terminal");
        let error = workspace
            .reconcile_session_inventory(&SessionInventory::new(vec![id], vec![id]))
            .expect_err("duplicate must fail");
        assert!(error.to_string().contains("more than once"));
        assert_eq!(workspace.to_persisted_json().expect("serialize"), before);
    }

    fn state_value() -> Value {
        json_value(&Workspace::mvp())
    }

    fn json_value(workspace: &Workspace) -> Value {
        serde_json::from_str(&workspace.to_persisted_json().expect("serialize")).expect("json")
    }

    fn assert_invalid(value: Value, expected: &str) {
        match Workspace::from_persisted_json(&value.to_string()) {
            Err(WorkspaceStateError::Invalid(message)) => assert!(
                message.contains(expected),
                "expected {expected:?} in {message:?}"
            ),
            other => panic!("expected invalid state, got {other:?}"),
        }
    }
}
