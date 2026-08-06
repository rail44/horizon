//! The folded item model and the event→state fold.

use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::event::{BoardEvent, Envelope};

/// A comment attached to an item. `author` is a free-form string
/// (convention: `owner` / `session:<uuid>` / future prefix-tagged forms).
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Comment {
    pub author: String,
    pub text: String,
    pub at: u64,
}

/// The full state of one work item after folding all events.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Item {
    pub id: u64,
    pub title: String,
    pub body: String,
    /// Free-form slug string (recommended vocabulary: proposed / ready /
    /// in-progress / review / done / blocked). Empty until first set.
    pub status: String,
    /// Lexicographic rank string (lowercase a-z).
    pub rank: String,
    /// Free-form assignee. Empty = unassigned.
    pub assignee: String,
    /// Optional parent item id.
    pub parent: Option<u64>,
    /// Items this one depends on.
    pub depends_on: Vec<u64>,
    /// Free-form links (session ids, branch names, doc paths, …).
    pub links: Vec<String>,
    /// Comments in chronological order.
    pub comments: Vec<Comment>,
}

/// Folds a chronologically-ordered slice of envelopes into a map of
/// item id → `Item`. Events referencing unknown items (e.g. an update
/// for an id whose `item-created` was in a corrupt/skipped line) are
/// silently dropped — the fold is a best-effort projection.
pub fn fold(envelopes: &[Envelope]) -> HashMap<u64, Item> {
    let mut items: HashMap<u64, Item> = HashMap::new();
    for env in envelopes {
        match &env.event {
            BoardEvent::ItemCreated {
                id,
                title,
                body,
                rank,
            } => {
                items.insert(
                    *id,
                    Item {
                        id: *id,
                        title: title.clone(),
                        body: body.clone(),
                        rank: rank.clone(),
                        ..Item::default()
                    },
                );
            }
            BoardEvent::ItemUpdated {
                id,
                status,
                rank,
                assignee,
                parent,
                depends_on,
                links,
                title,
                body,
            } => {
                if let Some(item) = items.get_mut(id) {
                    if let Some(v) = status {
                        item.status = v.clone();
                    }
                    if let Some(v) = rank {
                        item.rank = v.clone();
                    }
                    if let Some(v) = assignee {
                        item.assignee = v.clone();
                    }
                    if let Some(v) = parent {
                        item.parent = *v;
                    }
                    if let Some(v) = depends_on {
                        item.depends_on = v.clone();
                    }
                    if let Some(v) = links {
                        item.links = v.clone();
                    }
                    if let Some(v) = title {
                        item.title = v.clone();
                    }
                    if let Some(v) = body {
                        item.body = v.clone();
                    }
                }
            }
            BoardEvent::CommentAdded { id, author, text } => {
                if let Some(item) = items.get_mut(id) {
                    item.comments.push(Comment {
                        author: author.clone(),
                        text: text.clone(),
                        at: env.at,
                    });
                }
            }
        }
    }
    items
}

/// Returns items sorted by rank (lexicographic).
pub fn sorted_by_rank(items: &HashMap<u64, Item>) -> Vec<&Item> {
    let mut v: Vec<&Item> = items.values().collect();
    v.sort_by(|a, b| a.rank.cmp(&b.rank));
    v
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::event::{BoardEvent, Envelope, SCHEMA, VERSION};

    fn env(at: u64, event: BoardEvent) -> Envelope {
        Envelope {
            schema: SCHEMA.to_string(),
            version: VERSION,
            at,
            event,
        }
    }

    #[test]
    fn fold_create_update_comment_roundtrip() {
        let envelopes = vec![
            env(
                1000,
                BoardEvent::ItemCreated {
                    id: 1,
                    title: "Task".to_string(),
                    body: "Do thing".to_string(),
                    rank: "n".to_string(),
                },
            ),
            env(
                2000,
                BoardEvent::ItemUpdated {
                    id: 1,
                    status: Some("in-progress".to_string()),
                    rank: None,
                    assignee: Some("owner".to_string()),
                    parent: Some(Some(5)),
                    depends_on: None,
                    links: Some(vec!["branch-x".to_string()]),
                    title: None,
                    body: None,
                },
            ),
            env(
                3000,
                BoardEvent::CommentAdded {
                    id: 1,
                    author: "owner".to_string(),
                    text: "Started".to_string(),
                },
            ),
        ];

        let items = fold(&envelopes);
        let item = &items[&1];
        assert_eq!(item.title, "Task");
        assert_eq!(item.body, "Do thing");
        assert_eq!(item.rank, "n");
        assert_eq!(item.status, "in-progress");
        assert_eq!(item.assignee, "owner");
        assert_eq!(item.parent, Some(5));
        assert_eq!(item.links, vec!["branch-x"]);
        assert_eq!(item.comments.len(), 1);
        assert_eq!(item.comments[0].text, "Started");
        assert_eq!(item.comments[0].at, 3000);
    }

    #[test]
    fn fold_clears_parent_with_some_none() {
        let envelopes = vec![
            env(
                1000,
                BoardEvent::ItemCreated {
                    id: 1,
                    title: "T".to_string(),
                    body: String::new(),
                    rank: "n".to_string(),
                },
            ),
            env(
                2000,
                BoardEvent::ItemUpdated {
                    id: 1,
                    status: None,
                    rank: None,
                    assignee: None,
                    parent: Some(Some(3)),
                    depends_on: None,
                    links: None,
                    title: None,
                    body: None,
                },
            ),
            env(
                3000,
                BoardEvent::ItemUpdated {
                    id: 1,
                    status: None,
                    rank: None,
                    assignee: None,
                    parent: Some(None), // clear
                    depends_on: None,
                    links: None,
                    title: None,
                    body: None,
                },
            ),
        ];
        let items = fold(&envelopes);
        assert_eq!(items[&1].parent, None);
    }

    #[test]
    fn fold_unknown_status_and_author_dont_break() {
        let envelopes = vec![
            env(
                1000,
                BoardEvent::ItemCreated {
                    id: 1,
                    title: "T".to_string(),
                    body: String::new(),
                    rank: "n".to_string(),
                },
            ),
            env(
                2000,
                BoardEvent::ItemUpdated {
                    id: 1,
                    status: Some("weird-custom-status".to_string()),
                    rank: None,
                    assignee: None,
                    parent: None,
                    depends_on: None,
                    links: None,
                    title: None,
                    body: None,
                },
            ),
            env(
                3000,
                BoardEvent::CommentAdded {
                    id: 1,
                    author: "session:abc-123".to_string(),
                    text: "note".to_string(),
                },
            ),
        ];
        let items = fold(&envelopes);
        assert_eq!(items[&1].status, "weird-custom-status");
        assert_eq!(items[&1].comments[0].author, "session:abc-123");
    }

    #[test]
    fn fold_update_for_unknown_item_is_dropped() {
        let envelopes = vec![env(
            1000,
            BoardEvent::ItemUpdated {
                id: 99,
                status: Some("x".to_string()),
                rank: None,
                assignee: None,
                parent: None,
                depends_on: None,
                links: None,
                title: None,
                body: None,
            },
        )];
        let items = fold(&envelopes);
        assert!(items.is_empty());
    }
}
