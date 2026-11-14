mod validation;

use std::collections::{HashMap, HashSet};
use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::types::{
    LayoutChild, LayoutNode, Pane, PaneId, PluginPane, SessionKind, SplitAxis, Tab, TabId,
    Workspace, WorkspaceSession,
};
use crate::SessionId;

pub const WORKSPACE_STATE_VERSION: u32 = 1;

/// The sessions the runtimes run, each with its kind.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct SessionInventory {
    pub sessions: Vec<(SessionKind, SessionId)>,
}

impl SessionInventory {
    pub fn new(sessions: impl IntoIterator<Item = (SessionKind, SessionId)>) -> Self {
        Self {
            sessions: sessions.into_iter().collect(),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InventoryReconcile {
    pub removed: Vec<SessionId>,
    pub added: Vec<SessionId>,
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
    session_id: Option<SessionId>,
    plugin: PluginPaneState,
}

/// A pane's plugin: its name, the pane's title, and the state the plugin
/// keeps, as the plugin writes it.
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PluginPaneState {
    name: String,
    title: String,
    state: serde_json::Value,
}

impl From<&PluginPane> for PluginPaneState {
    fn from(plugin: &PluginPane) -> Self {
        Self {
            name: plugin.name.clone(),
            title: plugin.title.clone(),
            state: plugin.state.clone(),
        }
    }
}

impl From<PluginPaneState> for PluginPane {
    fn from(plugin: PluginPaneState) -> Self {
        Self {
            name: plugin.name,
            title: plugin.title,
            state: plugin.state,
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SessionState {
    id: SessionId,
    kind: SessionKindState,
    display_number: usize,
    title: String,
}

/// A session's persisted kind.
#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum SessionKindState {
    Terminal,
    Agent,
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
            self.register_detached_session(kind, id);
            added.push(id);
        }

        // Pruning every persisted pane away used to auto-create a fresh
        // terminal here so the workspace never reached zero tabs
        // (`WorkspaceState::validate` used to reject that). A zero-tab
        // workspace is now a valid, persistable state (2026-07-18 owner
        // clarification), so reconciliation simply leaves it empty.
        Ok(InventoryReconcile { removed, added })
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
        self.exit_workspace_mode();
    }
}

impl SessionInventory {
    fn validate(&self) -> Result<HashMap<SessionId, SessionKind>, InventoryError> {
        let mut result = HashMap::new();
        for (kind, id) in &self.sessions {
            if let Some(previous) = result.insert(*id, *kind) {
                return Err(InventoryError(format!(
                    "session {id:?} appears more than once ({} and {})",
                    previous.label(),
                    kind.label()
                )));
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
                    // Not persisted (see `WorkspaceSession::directory`'s
                    // doc comment) -- a restored session's directory
                    // starts unknown again.
                    directory: None,
                    // Same as `directory` above -- `WorkspaceSession::
                    // parent_session_id`'s doc comment.
                    parent_session_id: None,
                })
                .collect(),
            active_tab: self.active_tab,
            next_terminal_display_number: self.next_terminal_display_number,
            next_agent_display_number: self.next_agent_display_number,
            workspace_mode: Default::default(),
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
                        session_id: pane.session_id,
                        plugin: PluginPaneState::from(&pane.plugin),
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
                    session_id: pane.session_id,
                    plugin: PluginPane::from(pane.plugin),
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

    use crate::operations::Content;

    /// A view pane's plugin, for a test about the pane rather than the
    /// view.
    fn view() -> PluginPane {
        PluginPane {
            name: "preview".to_string(),
            title: "Preview".to_string(),
            state: Value::Null,
        }
    }

    use super::*;

    #[test]
    fn state_round_trip_preserves_layout_sessions_titles_and_counters() {
        let mut workspace = Workspace::mvp();
        let terminal = workspace.active_session_id().expect("terminal");
        let agent = SessionId::new();
        workspace.split_active(SessionKind::Agent, agent);
        workspace.register_detached_session(crate::SessionKind::Terminal, SessionId::new());
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
        assert_eq!(
            restored.workspace_mode,
            crate::mode::WorkspaceMode::PaneInput
        );
        assert_eq!(restored.to_persisted_json().expect("serialize again"), json);
    }

    #[test]
    fn a_zero_tab_workspace_round_trips() {
        // 2026-07-18 owner clarification: an empty workspace is a valid,
        // first-class state -- closing/terminating everything must not
        // leave the workspace unpersistable (the root cause `704657b`
        // originally worked around by auto-reseeding a terminal instead).
        let mut workspace = Workspace::mvp();
        workspace.terminate_active_session();
        assert_eq!(workspace.tab_count(), 0);

        let json = workspace
            .to_persisted_json()
            .expect("an empty workspace must serialize");
        let value: Value = serde_json::from_str(&json).expect("json");
        assert_eq!(value["tabs"], json!([]));

        let restored =
            Workspace::from_persisted_json(&json).expect("an empty workspace must restore");

        assert_eq!(restored.tab_count(), 0);
        assert_eq!(restored.panes.len(), 0);
        assert_eq!(restored.sessions, workspace.sessions);
        assert_eq!(restored.to_persisted_json().expect("serialize again"), json);
    }

    #[test]
    fn state_round_trip_preserves_a_view_pane_without_a_session() {
        let mut workspace = Workspace::mvp();
        let terminal_pane = workspace.visible_pane_id(0).expect("terminal pane");
        let view_pane = workspace.split_active_tab_with_view(view(), SplitAxis::Horizontal);

        let json = workspace.to_persisted_json().expect("serialize");
        let value: Value = serde_json::from_str(&json).expect("json");
        let pane = &value["tabs"][0]["root"]["children"][1]["node"]["pane"];
        assert_eq!(pane["plugin"]["name"], json!("preview"));
        assert!(pane.get("kind").is_none());
        assert!(value["tabs"][0]["root"]["children"][1]["node"]["pane"]["session_id"].is_null());

        let restored = Workspace::from_persisted_json(&json).expect("restore");

        assert_eq!(
            restored
                .pane_plugin(view_pane)
                .map(|plugin| plugin.name.as_str()),
            Some("preview")
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

    /// A plugin view keeps which plugin it shows and the plugin's state,
    /// so a restart brings back the same view on the same state.
    #[test]
    fn state_round_trip_preserves_a_plugin_pane_and_its_state() {
        let mut workspace = Workspace::mvp();
        let view_pane = workspace.split_active_tab_with_view(view(), SplitAxis::Horizontal);
        let plugin = PluginPane {
            name: "board-thread".to_string(),
            title: "Thread".to_string(),
            state: json!({"task": 42}),
        };
        assert!(workspace.set_pane_plugin(view_pane, plugin.clone()));

        let json = workspace.to_persisted_json().expect("serialize");
        let value: Value = serde_json::from_str(&json).expect("json");
        let pane = &value["tabs"][0]["root"]["children"][1]["node"]["pane"];
        assert!(pane.get("kind").is_none());
        assert_eq!(
            pane["plugin"],
            json!({"name": "board-thread", "title": "Thread", "state": {"task": 42}})
        );
        assert!(pane["session_id"].is_null());
        // A session's pane shows its kind's plugin.
        let terminal = &value["tabs"][0]["root"]["children"][0]["node"]["pane"];
        assert_eq!(terminal["plugin"]["name"], json!("terminal"));

        let restored = Workspace::from_persisted_json(&json).expect("restore");
        assert_eq!(restored.pane_plugin(view_pane), Some(&plugin));
        assert_eq!(
            restored.pane_title_for(view_pane).as_deref(),
            Some("Thread")
        );
        assert_eq!(restored.to_persisted_json().expect("serialize again"), json);
    }

    /// A plugin view without its plugin, or another pane with one, is not
    /// a state this model writes or restores.
    #[test]
    fn only_a_plugin_view_restores_with_a_plugin() {
        let mut workspace = Workspace::mvp();
        workspace.split_active_tab_with_view(view(), SplitAxis::Horizontal);
        let json = workspace.to_persisted_json().expect("serialize");
        let value: Value = serde_json::from_str(&json).expect("json");
        let edited = |edit: &dyn Fn(&mut Value)| {
            let mut value = value.clone();
            edit(&mut value);
            serde_json::to_string(&value).expect("serialize the edit")
        };
        let view_without = edited(&|value| {
            let pane = &mut value["tabs"][0]["root"]["children"][1]["node"]["pane"];
            pane.as_object_mut().expect("a pane").remove("plugin");
        });
        assert!(Workspace::from_persisted_json(&view_without).is_err());
        let terminal_with = edited(&|value| {
            value["tabs"][0]["root"]["children"][0]["node"]["pane"]["plugin"] =
                json!({"name": "board-list", "title": "Board", "state": null});
        });
        assert!(Workspace::from_persisted_json(&terminal_with).is_err());
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
        workspace.split_active(SessionKind::Terminal, SessionId::new());
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
        workspace.split_active(SessionKind::Agent, SessionId::new());

        let mut duplicate = json_value(&workspace);
        let first_id = duplicate["sessions"][0]["id"].clone();
        duplicate["sessions"][1]["id"] = first_id;
        assert_invalid(duplicate, "duplicate session id");

        let mut mismatch = json_value(&workspace);
        mismatch["tabs"][0]["root"]["children"][1]["node"]["pane"]["plugin"]["name"] =
            json!("terminal");
        assert_invalid(mismatch, "is of kind agent");

        let mut counter = json_value(&workspace);
        counter["next_terminal_display_number"] = json!(1);
        assert_invalid(counter, "counter");
    }

    #[test]
    fn validation_preserves_session_then_layout_then_attachment_error_order() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(SessionKind::Terminal, SessionId::new());
        let mut value = json_value(&workspace);
        value["sessions"][0]["title"] = json!("");
        value["tabs"][0]["root"]["children"][0]["weight"] = json!(0.0);
        value["tabs"][0]["root"]["children"][0]["node"]["pane"]["session_id"] = Value::Null;
        assert_invalid(value.clone(), "session titles must not be empty");
        value["sessions"][0]["title"] = json!("restored");
        assert_invalid(value.clone(), "weights");
        value["tabs"][0]["root"]["children"][0]["weight"] = json!(1.0);
        assert_invalid(value, "no session attachment");
    }

    #[test]
    fn validation_rejects_a_session_shared_by_distinct_panes() {
        let mut workspace = Workspace::mvp();
        workspace.split_active(SessionKind::Terminal, SessionId::new());
        let mut value = json_value(&workspace);
        let first = value["tabs"][0]["root"]["children"][0]["node"]["pane"]["session_id"].clone();
        value["tabs"][0]["root"]["children"][1]["node"]["pane"]["session_id"] = first;
        assert_invalid(value, "attached to multiple panes");
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
        workspace.split_active_tab_with_view(view(), SplitAxis::Horizontal);
        let mut value = json_value(&workspace);
        // Any session id is rejected here regardless of whether it's
        // known -- a view pane must have none at all.
        let borrowed_session_id = value["sessions"][0]["id"].clone();
        value["tabs"][0]["root"]["children"][1]["node"]["pane"]["session_id"] = borrowed_session_id;
        assert_invalid(value, "shows preview but its session");
    }

    #[test]
    fn inventory_prunes_missing_panes_collapses_tree_and_repairs_active_state() {
        let mut workspace = Workspace::mvp();
        let retained = workspace.active_session_id().expect("terminal");
        let missing_agent = SessionId::new();
        workspace.split_active(SessionKind::Agent, missing_agent);
        let second_tab = SessionId::new();
        workspace.open_tab(Content::Session(SessionKind::Terminal, second_tab));
        let daemon_only = SessionId::new();

        let outcome = workspace
            .reconcile_session_inventory(&SessionInventory::new([
                (SessionKind::Terminal, retained),
                (SessionKind::Terminal, daemon_only),
            ]))
            .expect("reconcile");

        assert_eq!(outcome.removed.len(), 2);
        assert!(outcome.removed.contains(&missing_agent));
        assert!(outcome.removed.contains(&second_tab));
        assert_eq!(outcome.added, vec![daemon_only]);
        assert_eq!(workspace.tabs.len(), 1);
        assert!(matches!(workspace.tabs[0].root, LayoutNode::Pane(_)));
        assert_eq!(workspace.active_session_id(), Some(retained));
        assert_eq!(workspace.detached_session_count(), 1);
    }

    #[test]
    fn inventory_preserves_detached_metadata_and_leaves_the_workspace_empty_when_all_panes_are_gone(
    ) {
        // 2026-07-18 owner clarification: pruning every persisted pane away
        // used to auto-create a fresh terminal (`created_terminal`, now
        // removed) so the workspace never reached zero tabs. A zero-tab
        // workspace is now a valid state, so reconciliation simply leaves
        // it empty instead of reseeding one.
        let mut workspace = Workspace::mvp();
        let attached = workspace.active_session_id().expect("terminal");
        let detached = SessionId::new();
        workspace.register_detached_session(crate::SessionKind::Agent, detached);
        let detached_before = workspace.session(detached).expect("detached").clone();

        let outcome = workspace
            .reconcile_session_inventory(&SessionInventory::new([(SessionKind::Agent, detached)]))
            .expect("reconcile");

        assert_eq!(outcome.removed, vec![attached]);
        assert!(outcome.added.is_empty());
        assert_eq!(workspace.tab_count(), 0);
        assert_eq!(workspace.active_session_id(), None);
        assert_eq!(workspace.session(detached), Some(&detached_before));
        assert_eq!(workspace.detached_session_count(), 1);
        assert!(workspace.to_persisted_json().is_ok());
    }

    #[test]
    fn invalid_inventory_is_atomic() {
        let mut workspace = Workspace::mvp();
        let before = workspace.to_persisted_json().expect("serialize");
        let id = workspace.active_session_id().expect("terminal");
        let error = workspace
            .reconcile_session_inventory(&SessionInventory::new([
                (SessionKind::Terminal, id),
                (SessionKind::Agent, id),
            ]))
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
