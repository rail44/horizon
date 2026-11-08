//! What the wiki gives agents, as plain data and functions a composition
//! root wraps (`horizon-agentd` registers them with `horizon-agent`): the
//! page tools, the role a thread's session runs with, the input that hands a
//! comment to it, and the derived ids that tie a thread to its session.

use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::annotation::{AgentType, Annotation, Target};
use crate::page::{body, Edit};
use crate::thread::Thread;
use crate::{Store, WikiError};

/// The namespace the wiki's derived ids are made in (UUID v5).
const NAMESPACE: Uuid = Uuid::from_u128(0x6f3c_2d5e_8a41_4b7f_9c0e_51d2_a7b8_3e64);

/// A stable id for `name`: the same name always gives the same id.
pub fn derived_id(name: &str) -> Uuid {
    Uuid::new_v5(&NAMESPACE, name.as_bytes())
}

/// The session that answers the thread whose root annotation is `root`.
pub fn thread_session(root: &str) -> Uuid {
    derived_id(&format!("thread-session:{root}"))
}

/// The id of the reply recorded for answer `answer` of session `session`,
/// so a reply delivered twice is recorded once.
pub fn reply_id(session: Uuid, answer: &str) -> Uuid {
    derived_id(&format!("reply:{session}:{answer}"))
}

pub const THREAD_ROLE_ID: &str = "wiki-thread";
pub const THREAD_ROLE_TITLE: &str = "Wiki Thread";

/// Read-only access to the repository and the web, plus the wiki's own
/// tools: a thread's session does not change the repository
/// (docs/project-wiki-design.md, "Comments and the wiki view").
pub const THREAD_ROLE_TOOLS: &[&str] = &[
    "fs.read",
    "fs.grep",
    "fs.glob",
    "recall.search",
    "recall.read",
    "recall.sessions",
    "knowledge.read",
    "skill.read",
    "task",
    "web_search",
    "web_fetch",
    "wiki.list",
    "wiki.read",
    "wiki.write",
    "wiki.edit",
];

pub const THREAD_ROLE_SKILLS: &[&str] = &["project-wiki"];

pub const THREAD_ROLE_PROMPT: &str = "You answer the owner's comments on the project wiki: \
     human-facing pages, kept outside the repository, that describe the project's current \
     state. Each message names a page, the passage the comment is on, and the owner's \
     question or remark; later messages continue the same thread.\n\
     \n\
     Read the page with `wiki.read` and whatever code, docs or history the answer needs. Your \
     final message is posted to the thread as your reply, so make it the answer itself, in the \
     owner's language, citing files where they help. Do not narrate your steps.\n\
     \n\
     When the comment shows the page is wrong, unclear or missing something, fix the page with \
     `wiki.edit` (or `wiki.write` for a new page), following the `project-wiki` skill's page \
     format, and say in your reply what you changed. You cannot change the repository: when \
     the answer needs a code or docs change, describe it for the owner instead.";

/// The page tools, as `(id, title, description, input schema)`.
pub fn tool_definitions() -> Vec<(&'static str, &'static str, &'static str, Value)> {
    vec![
        (
            "wiki.list",
            "List wiki pages",
            "List the project wiki's pages: name, title and id.",
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        ),
        (
            "wiki.read",
            "Read a wiki page",
            "Read one project wiki page (frontmatter included), or `concepts` for the concept \
             file.",
            json!({
                "type": "object",
                "properties": {"page": {"type": "string", "description": "Page name, e.g. `index`"}},
                "required": ["page"],
                "additionalProperties": false
            }),
        ),
        (
            "wiki.write",
            "Write a wiki page",
            "Write a project wiki page whole, creating it if it does not exist. The text opens \
             with a `---` frontmatter block; leave out `id` and the page keeps its own. Use \
             `wiki.edit` to change part of an existing page.",
            json!({
                "type": "object",
                "properties": {
                    "page": {"type": "string", "description": "Page name: lowercase letters, digits, hyphens"},
                    "text": {"type": "string"}
                },
                "required": ["page", "text"],
                "additionalProperties": false
            }),
        ),
        (
            "wiki.edit",
            "Edit a wiki page",
            "Replace exact strings in a project wiki page. Each `old` must occur exactly once in \
             the page as it stands after the edits before it; all edits apply or none do.",
            json!({
                "type": "object",
                "properties": {
                    "page": {"type": "string"},
                    "edits": {
                        "type": "array",
                        "minItems": 1,
                        "items": {
                            "type": "object",
                            "properties": {"old": {"type": "string"}, "new": {"type": "string"}},
                            "required": ["old", "new"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["page", "edits"],
                "additionalProperties": false
            }),
        ),
    ]
}

#[derive(Deserialize)]
struct PageInput {
    page: String,
}

#[derive(Deserialize)]
struct WriteInput {
    page: String,
    text: String,
}

#[derive(Deserialize)]
struct EditInput {
    page: String,
    edits: Vec<Edit>,
}

/// Runs page tool `tool_id` against `store`. `Err` is a message for the
/// model.
pub fn run_tool(store: &Store, tool_id: &str, input: &Value) -> Result<Value, String> {
    let parse = |error: serde_json::Error| format!("invalid input: {error}");
    let result = match tool_id {
        "wiki.list" => {
            let pages = store.list_pages().map_err(message)?;
            json!({"store": store.dir(), "pages": pages.iter().map(|page| json!({
                "name": page.name,
                "title": page.title,
                "id": page.id.map(crate::page::urn),
            })).collect::<Vec<_>>()})
        }
        "wiki.read" => {
            let PageInput { page } = PageInput::deserialize(input).map_err(parse)?;
            let text = if page == "concepts" {
                std::fs::read_to_string(store.concepts_path()).map_err(|error| error.to_string())?
            } else {
                store.read_page(&page).map_err(message)?
            };
            json!({"page": page, "text": text})
        }
        "wiki.write" => {
            let WriteInput { page, text } = WriteInput::deserialize(input).map_err(parse)?;
            let id = store.write_page(&page, &text).map_err(message)?;
            json!({"page": page, "id": crate::page::urn(id), "written": true})
        }
        "wiki.edit" => {
            let EditInput { page, edits } = EditInput::deserialize(input).map_err(parse)?;
            store.edit_page(&page, &edits).map_err(message)?;
            json!({"page": page, "edited": edits.len()})
        }
        other => return Err(format!("`{other}` is not a wiki tool")),
    };
    Ok(result)
}

fn message(error: WikiError) -> String {
    error.to_string()
}

/// The annotations in `thread` the owner made that no session has replied
/// to yet, in thread order.
pub fn unanswered(thread: &Thread) -> Vec<&Annotation> {
    let all: Vec<&Annotation> = std::iter::once(&thread.root)
        .chain(&thread.replies)
        .collect();
    all.iter()
        .copied()
        .filter(|annotation| annotation.creator.kind == AgentType::Person)
        .filter(|asked| {
            !all.iter().any(|reply| {
                reply.creator.kind == AgentType::Software
                    && matches!(&reply.target, Target::Annotation(parent) if *parent == asked.id)
            })
        })
        .collect()
}

/// The session input text for `annotation` in `thread` on page `page`
/// (its name). The thread's first message carries the quoted passage.
pub fn thread_input(store: &Store, page: &str, thread: &Thread, annotation: &Annotation) -> String {
    let question = &annotation.body.value;
    if annotation.id != thread.root.id {
        return format!("Follow-up in the thread on wiki page `{page}`:\n\n{question}");
    }
    let quote = annotation.quote();
    let passage = quote.as_ref().map_or(String::new(), |quote| {
        format!(
            "\nThe comment is on this passage (characters {}..{} of the page's text):\n\n> {}\n",
            quote.start,
            quote.end,
            quote.exact.replace('\n', "\n> ")
        )
    });
    let current = store
        .read_page(page)
        .ok()
        .map(|text| crate::logical::LogicalText::of_markdown(body(&text)))
        .zip(quote.as_ref())
        .map(|(logical, quote)| crate::anchor::anchor(&logical, quote));
    let moved = match current {
        Some(crate::anchor::Anchor::Orphaned) => {
            "\nThe page has changed since, and the passage is no longer in it.\n"
        }
        Some(crate::anchor::Anchor::Moved { exact: false, .. }) => {
            "\nThe page has changed since; the passage reads differently now.\n"
        }
        _ => "",
    };
    format!("The owner commented on wiki page `{page}`.\n{passage}{moved}\n{question}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::annotation::{markdown_body, Agent};

    #[test]
    fn derived_ids_are_stable_and_distinct() {
        assert_eq!(thread_session("urn:uuid:a"), thread_session("urn:uuid:a"));
        assert_ne!(thread_session("urn:uuid:a"), thread_session("urn:uuid:b"));
        let session = Uuid::new_v4();
        assert_ne!(reply_id(session, "1"), reply_id(session, "2"));
    }

    #[test]
    fn a_question_stays_unanswered_until_a_session_replies_to_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        store
            .write_page("p", "---\ntitle: P\n---\nThe helper runs nono first.\n")
            .unwrap();
        let owner = Agent::person(Uuid::new_v4(), "owner");
        let session = Agent::session(Uuid::new_v4());
        let (page, question) = store
            .ask("p", 4..10, owner.clone(), markdown_body("Which helper?"))
            .unwrap();
        let follow_up = store
            .reply(page, &question.id, owner, markdown_body("And why?"))
            .unwrap();
        let thread = &store.threads(page).unwrap()[0];
        assert_eq!(unanswered(thread), vec![&question, &follow_up]);
        store
            .reply(
                page,
                &question.id,
                session,
                markdown_body("The sandbox one."),
            )
            .unwrap();
        let thread = &store.threads(page).unwrap()[0];
        assert_eq!(unanswered(thread), vec![&follow_up]);

        let first = thread_input(&store, "p", thread, &question);
        assert!(first.contains("> helper"), "{first}");
        assert!(first.ends_with("Which helper?"), "{first}");
        let later = thread_input(&store, "p", thread, &follow_up);
        assert!(later.starts_with("Follow-up"), "{later}");
    }

    #[test]
    fn the_tools_read_write_and_edit_pages() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::at(dir.path());
        run_tool(
            &store,
            "wiki.write",
            &json!({"page": "p", "text": "---\ntitle: P\n---\nold text\n"}),
        )
        .unwrap();
        run_tool(
            &store,
            "wiki.edit",
            &json!({"page": "p", "edits": [{"old": "old", "new": "new"}]}),
        )
        .unwrap();
        let read = run_tool(&store, "wiki.read", &json!({"page": "p"})).unwrap();
        assert!(read["text"].as_str().unwrap().ends_with("new text\n"));
        let list = run_tool(&store, "wiki.list", &json!({})).unwrap();
        assert_eq!(list["pages"][0]["name"], "p");
        assert!(run_tool(&store, "wiki.read", &json!({"page": "../x"})).is_err());
        assert!(run_tool(
            &store,
            "wiki.edit",
            &json!({"page": "p", "edits": [{"old": "zz", "new": ""}]})
        )
        .is_err());
    }
}
