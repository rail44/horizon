//! Comments as W3C Web Annotation Data Model annotations (Recommendation,
//! 2017-02-23), the subset the wiki uses (docs/project-wiki-design.md,
//! "Comments and the wiki view"): plain JSON carrying the `@context` line,
//! every identifier a `urn:uuid:` IRI.
//!
//! A question targets a range of a page: a `SpecificResource` whose source is
//! the page's id, selected by a `TextQuoteSelector` (primary) and a
//! `TextPositionSelector` (to tell repeated quotes apart) over the page's
//! logical text ([`crate::logical`]), with a `TimeState` naming the snapshot
//! of the page the comment was made on. A reply targets its parent
//! annotation's id.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const CONTEXT: &str = "http://www.w3.org/ns/anno.jsonld";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    #[serde(rename = "@context")]
    pub context: String,
    pub id: String,
    #[serde(rename = "type")]
    pub kind: AnnotationType,
    pub motivation: Motivation,
    pub creator: Agent,
    pub created: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<String>,
    pub body: TextualBody,
    pub target: Target,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AnnotationType {
    Annotation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Motivation {
    Commenting,
    Questioning,
    Replying,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Agent {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: AgentType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AgentType {
    Person,
    Software,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextualBody {
    #[serde(rename = "type")]
    pub kind: TextualBodyType,
    pub value: String,
    pub format: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextualBodyType {
    TextualBody,
}

/// A range of a page, or (for a reply) the parent annotation's id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Target {
    Range(SpecificResource),
    Annotation(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpecificResource {
    #[serde(rename = "type")]
    pub kind: SpecificResourceType,
    pub source: String,
    pub selector: Vec<Selector>,
    pub state: TimeState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpecificResourceType {
    SpecificResource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Selector {
    TextQuoteSelector {
        exact: String,
        prefix: String,
        suffix: String,
    },
    TextPositionSelector {
        start: usize,
        end: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeState {
    #[serde(rename = "type")]
    pub kind: TimeStateType,
    #[serde(rename = "sourceDate")]
    pub source_date: String,
    /// The snapshot of the page this annotation was made on.
    pub cached: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeStateType {
    TimeState,
}

/// Where a question points in a page's logical text, in code points.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    pub exact: String,
    pub prefix: String,
    pub suffix: String,
    pub start: usize,
    pub end: usize,
}

/// Context kept on each side of a quote, as Hypothesis keeps.
pub const CONTEXT_CHARS: usize = 32;

impl Quote {
    /// The quote of `chars` in `logical`, with up to [`CONTEXT_CHARS`] of
    /// context on each side. `None` for an empty or out-of-range selection.
    pub fn of(
        logical: &crate::logical::LogicalText,
        chars: std::ops::Range<usize>,
    ) -> Option<Self> {
        if chars.is_empty() || chars.end > logical.len() {
            return None;
        }
        Some(Self {
            exact: logical.slice(chars.clone()),
            prefix: logical.slice(chars.start.saturating_sub(CONTEXT_CHARS)..chars.start),
            suffix: logical.slice(chars.end..(chars.end + CONTEXT_CHARS).min(logical.len())),
            start: chars.start,
            end: chars.end,
        })
    }
}

pub fn urn(id: Uuid) -> String {
    crate::page::urn(id)
}

pub fn markdown_body(value: impl Into<String>) -> TextualBody {
    TextualBody {
        kind: TextualBodyType::TextualBody,
        value: value.into(),
        format: "text/markdown".into(),
        language: None,
    }
}

impl Annotation {
    /// A question (or comment) on `quote` in page `page`, made on
    /// `snapshot`'s copy of it.
    pub fn on_range(
        motivation: Motivation,
        page: Uuid,
        quote: &Quote,
        snapshot: Uuid,
        creator: Agent,
        body: TextualBody,
    ) -> Self {
        let created = now_utc();
        Self {
            context: CONTEXT.into(),
            id: urn(Uuid::new_v4()),
            kind: AnnotationType::Annotation,
            motivation,
            creator,
            created: created.clone(),
            modified: None,
            body,
            target: Target::Range(SpecificResource {
                kind: SpecificResourceType::SpecificResource,
                source: urn(page),
                selector: vec![
                    Selector::TextQuoteSelector {
                        exact: quote.exact.clone(),
                        prefix: quote.prefix.clone(),
                        suffix: quote.suffix.clone(),
                    },
                    Selector::TextPositionSelector {
                        start: quote.start,
                        end: quote.end,
                    },
                ],
                state: TimeState {
                    kind: TimeStateType::TimeState,
                    source_date: created,
                    cached: urn(snapshot),
                },
            }),
        }
    }

    /// A reply to the annotation `parent` (its id).
    pub fn reply(parent: &str, creator: Agent, body: TextualBody) -> Self {
        Self::reply_with_id(Uuid::new_v4(), parent, creator, body)
    }

    /// A reply with a given id, for a writer that may record the same reply
    /// twice and relies on the log refusing a repeated id.
    pub fn reply_with_id(id: Uuid, parent: &str, creator: Agent, body: TextualBody) -> Self {
        Self {
            context: CONTEXT.into(),
            id: urn(id),
            kind: AnnotationType::Annotation,
            motivation: Motivation::Replying,
            creator,
            created: now_utc(),
            modified: None,
            body,
            target: Target::Annotation(parent.to_string()),
        }
    }

    /// The quote and position this annotation's target selects, when it
    /// targets a range.
    pub fn quote(&self) -> Option<Quote> {
        let Target::Range(resource) = &self.target else {
            return None;
        };
        let mut quote = None;
        let mut position = None;
        for selector in &resource.selector {
            match selector {
                Selector::TextQuoteSelector {
                    exact,
                    prefix,
                    suffix,
                } => quote = Some((exact.clone(), prefix.clone(), suffix.clone())),
                Selector::TextPositionSelector { start, end } => position = Some((*start, *end)),
            }
        }
        let (exact, prefix, suffix) = quote?;
        let (start, end) = position.unwrap_or((0, 0));
        Some(Quote {
            exact,
            prefix,
            suffix,
            start,
            end,
        })
    }
}

impl Agent {
    pub fn person(id: Uuid, nickname: impl Into<String>) -> Self {
        Self {
            id: urn(id),
            kind: AgentType::Person,
            name: None,
            nickname: Some(nickname.into()),
        }
    }

    /// An agent session, identified by its session id.
    pub fn session(session_id: Uuid) -> Self {
        Self {
            id: urn(session_id),
            kind: AgentType::Software,
            name: Some("Horizon agent session".into()),
            nickname: None,
        }
    }
}

/// The current time as `xsd:dateTime` in UTC with `Z`, seconds precision.
pub fn now_utc() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs());
    format_utc(secs)
}

fn format_utc(secs: u64) -> String {
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utc_formatting_matches_known_instants() {
        assert_eq!(format_utc(0), "1970-01-01T00:00:00Z");
        assert_eq!(format_utc(951_782_400), "2000-02-29T00:00:00Z");
        assert_eq!(format_utc(1_794_268_799), "2026-11-09T23:59:59Z");
    }

    #[test]
    fn a_quote_keeps_context_on_both_sides() {
        let logical = crate::logical::LogicalText::of_markdown("abcdef");
        let quote = Quote::of(&logical, 2..4).unwrap();
        assert_eq!(
            (
                quote.prefix.as_str(),
                quote.exact.as_str(),
                quote.suffix.as_str()
            ),
            ("ab", "cd", "ef")
        );
        assert!(Quote::of(&logical, 3..3).is_none());
        assert!(Quote::of(&logical, 4..9).is_none());
    }
}
