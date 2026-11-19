//! What the list is made of, with no element in it: the sessions by
//! project, each project's in the order its owner would look at them,
//! and what a search finds.

use std::collections::{HashMap, HashSet};

use horizon_ui::guest::bindings::horizon::ui::sessions::{Attention, Session};

/// A session's row: the session, and how far under what it was derived
/// from it sits.
#[derive(Clone, PartialEq)]
pub struct Row {
    pub session: Session,
    pub depth: usize,
}

/// A project's sessions.
#[derive(Clone, PartialEq)]
pub struct Group {
    /// What tells the project from another.
    pub root: String,
    pub name: String,
    pub rows: Vec<Row>,
}

/// How soon a session is to be looked at: what waits on its owner first,
/// then what was finished and not looked at, then what works, then what
/// rests.
fn rank(session: &Session) -> u8 {
    match session.activity.as_ref() {
        Some(activity) => match activity.attention {
            Some(Attention::Unseen) => 1,
            Some(_) => 0,
            None if activity.working => 2,
            None => 3,
        },
        None => 3,
    }
}

/// Whether a session waits on its owner: for something to be done in it,
/// or for what it finished or failed at to be looked at.
pub fn waits(session: &Session) -> bool {
    (session.activity.as_ref()).is_some_and(|activity| activity.attention.is_some())
}

/// The word a row's chip says, for a state that has one.
pub fn chip(session: &Session) -> Option<&'static str> {
    let activity = session.activity.as_ref()?;
    Some(match activity.attention {
        Some(Attention::Approval) => "approval",
        Some(Attention::Input) => "input",
        Some(Attention::Login) => "login",
        Some(Attention::Blocked) => "blocked",
        Some(Attention::Failed) => "failed",
        Some(Attention::Unseen) => "unseen",
        None if activity.working => "working",
        None => return None,
    })
}

/// What a session is, in a word: the name the program in a terminal
/// gives itself, or the session's kind.
pub fn kind(session: &Session) -> &str {
    (session.activity.as_ref())
        .and_then(|activity| activity.app.as_deref())
        .unwrap_or(&session.kind)
}

/// Whether every word of `query` is somewhere in what a row says of its
/// session: its title, kind, state, words, or project.
fn found(session: &Session, query: &[String]) -> bool {
    let activity = session.activity.as_ref();
    let said = [
        Some(session.title.as_str()),
        Some(kind(session)),
        Some(session.kind.as_str()),
        activity.map(|activity| activity.state.as_str()),
        chip(session),
        activity.and_then(|activity| activity.detail.as_deref()),
        session
            .project
            .as_ref()
            .map(|project| project.name.as_str()),
    ];
    let said: Vec<String> = (said.into_iter().flatten())
        .map(str::to_lowercase)
        .collect();
    (query.iter()).all(|word| said.iter().any(|said| said.contains(word)))
}

/// The sessions `query` finds, by project.
///
/// A session sits under the one it was derived from, and the two are
/// placed by the more pressing of them; among those as pressing, the one
/// that came to its state last is first. A project is where the first of
/// its sessions was made among the others', so that a project stays
/// where it is as its sessions change.
pub fn groups(sessions: &[Session], query: &str) -> Vec<Group> {
    let query: Vec<String> = (query.split_whitespace()).map(str::to_lowercase).collect();
    let known: HashSet<&str> = sessions.iter().map(|session| session.id.as_str()).collect();
    let mut derived: HashMap<&str, Vec<&Session>> = HashMap::new();
    let mut roots = Vec::new();
    for session in sessions {
        match session
            .parent
            .as_deref()
            .filter(|parent| known.contains(parent))
        {
            // (A session derived from itself, or round a ring, is a root.)
            Some(parent) if parent != session.id => {
                derived.entry(parent).or_default().push(session)
            }
            _ => roots.push(session),
        }
    }

    /// A root's rows, depth first, and the most pressing rank among them.
    fn tree<'a>(
        root: &'a Session,
        depth: usize,
        derived: &HashMap<&str, Vec<&'a Session>>,
        seen: &mut HashSet<&'a str>,
        rows: &mut Vec<Row>,
    ) -> u8 {
        if !seen.insert(root.id.as_str()) {
            return u8::MAX;
        }
        rows.push(Row {
            session: root.clone(),
            depth,
        });
        let mut pressing = rank(root);
        for child in derived.get(root.id.as_str()).into_iter().flatten() {
            pressing = pressing.min(tree(child, depth + 1, derived, seen, rows));
        }
        pressing
    }

    let mut seen = HashSet::new();
    let mut groups: Vec<Group> = Vec::new();
    let mut trees: Vec<(usize, u8, u64, Vec<Row>)> = Vec::new();
    for root in roots {
        let mut rows = Vec::new();
        let pressing = tree(root, 0, &derived, &mut seen, &mut rows);
        // A search keeps a session with what it was derived from, down to
        // the last one found: what is found is not shown out of its place.
        if !query.is_empty() && !rows.iter().any(|row| found(&row.session, &query)) {
            continue;
        }
        let (project, name) = match &root.project {
            Some(project) => (project.root.clone(), project.name.clone()),
            None => (String::new(), "No directory".to_string()),
        };
        let group = match groups.iter().position(|group| group.root == project) {
            Some(group) => group,
            None => {
                groups.push(Group {
                    root: project,
                    name,
                    rows: Vec::new(),
                });
                groups.len() - 1
            }
        };
        let since = (root.activity.as_ref()).map_or(0, |activity| activity.since_ms);
        trees.push((group, pressing, since, rows));
    }
    // (Stable: what is as pressing and as recent stays in the order made.)
    trees.sort_by_key(|tree| (tree.1, std::cmp::Reverse(tree.2)));
    for (group, _, _, rows) in trees {
        groups[group].rows.extend(rows);
    }
    groups
}

/// How long ago `since_ms` was at `now_ms`, in a few characters.
pub fn ago(since_ms: u64, now_ms: u64) -> String {
    let minutes = now_ms.saturating_sub(since_ms) / 60_000;
    match minutes {
        0 => "now".to_string(),
        1..60 => format!("{minutes}m"),
        60..1440 => format!("{}h", minutes / 60),
        1440..10080 => format!("{}d", minutes / 1440),
        _ => format!("{}w", minutes / 10080),
    }
}
