//! Project knowledge: a user-side store of distilled lessons keyed by
//! project root, with an always-loaded prompt index and two tools
//! (`knowledge.read` / `knowledge.write`).
//!
//! **Design** (owner-approved 2026-08-05). The store lives outside any
//! repository, under the user's data home:
//! `$XDG_DATA_HOME/horizon/knowledge/<sanitized-root>/<id>.md` (same
//! XDG-resolution shape as `config::default_event_log_path_from`), keyed
//! by the project's *main* root (the `--git-common-dir` parent, not the
//! worktree's own toplevel) so every worktree of one project shares the
//! same knowledge. One file per entry; YAML-ish `---`-delimited
//! frontmatter (`id`/`description`/`anchors?`/`sources`/`created`/
//! `updated`/`status`), then a free-form Markdown body.
//!
//! **Trust gate.** Both the prompt index and the two tools are withheld
//! from sessions whose project root is not in the user's
//! `trusted_projects` list — the same `trusted_project` flag that
//! already gates `AGENTS.md`/`CLAUDE.md` instructions and repository
//! skills (see `skills`' module doc). The index is simply not appended
//! to the system prompt for an untrusted session, and the two tools are
//! filtered out of the advertised catalog by `rig_tool_definitions`
//! (which receives `trusted_project` from `RigAgentConfig`).
//!
//! **Three disclosures**, matching `skills`' shape:
//! 1. **Always loaded**: a prompt section listing `id: description` for
//!    every `status: active` entry, capped at 16_000 characters (entries
//!    adopted in `updated`-descending order when the cap is exceeded).
//! 2. **Read on demand**: `knowledge.read` returns one entry's
//!    frontmatter + body, re-read from disk each call (same "edit ->
//!    observe" loop as `skill.read`).
//! 3. **Write on demand**: `knowledge.write` upserts an entry. No
//!    approval (the tool-event recording is the audit); validation
//!    rejects a non-slug `id`, an empty `description`, or an empty
//!    `sources` list.

use std::path::{Path, PathBuf};

use crate::instructions::cap_to_chars;

// --- constants -----------------------------------------------------------

/// Cap for the always-loaded prompt index, in characters. Entries are
/// adopted in `updated`-descending order until the section would exceed
/// this. A plain constant (not a config knob) — same rationale as
/// `skills::SKILL_BODY_CAP_CHARS`.
const KNOWLEDGE_INDEX_CAP_CHARS: usize = 16_000;

/// Cap for a single entry's body as returned by `knowledge.read`, in
/// characters. Matches `skills::SKILL_BODY_CAP_CHARS` — a knowledge
/// entry is arbitrary user-authored content, so it needs the same
/// size-cap discipline.
const KNOWLEDGE_BODY_CAP_CHARS: usize = 24_000;

// --- path resolution -----------------------------------------------------

/// Resolves the user's data home (`$XDG_DATA_HOME`, falling back to
/// `~/.local/share`, then the OS temp dir), delegating to `config`'s
/// resolver so the fallback chain stays identical to the event log's
/// and DuckDB projection's built-in defaults.
fn agent_data_home() -> PathBuf {
    crate::config::agent_data_home()
}

/// Sanitizes a project root's absolute path into a directory-safe
/// segment: every `/` (including the leading one) becomes `-`. E.g.
/// `/home/user/src/project` → `-home-user-src-project`.
fn sanitize_root(root: &Path) -> String {
    root.to_string_lossy().replace('/', "-")
}

/// The store directory for a project whose main root is `root`:
/// `<data-home>/horizon/knowledge/<sanitized-root>`.
fn store_dir_for_root(root: &Path) -> PathBuf {
    agent_data_home()
        .join("horizon")
        .join("knowledge")
        .join(sanitize_root(root))
}

// --- main-root resolution -------------------------------------------------

/// Strips inherited `GIT_*` environment variables from `cmd`. Git honors
/// several (`GIT_DIR`, `GIT_WORK_TREE`, `GIT_COMMON_DIR`, ...) as
/// overrides that take precedence over `-C`. A duplicate of
/// `horizon-agentd`'s `worktree::scrub_git_env` — this crate can't
/// depend on that module, and the same backlog-53 hazard (hook-exported
/// `GIT_DIR` silently redirecting an invocation) applies here too.
fn scrub_git_env(cmd: &mut std::process::Command) {
    for (key, _) in std::env::vars() {
        // Preserve `GIT_CEILING_DIRECTORIES` — it doesn't redirect to a
        // different repo (the backlog-53 hazard), it prevents git from
        // walking up past a boundary, which is safe and useful in tests
        // where TMPDIR may sit inside a git repo.
        if key.starts_with("GIT_") && key != "GIT_CEILING_DIRECTORIES" {
            cmd.env_remove(key);
        }
    }
}

/// The project's *main* repository root (canonicalized), resolved from
/// `dir` via `git rev-parse --git-common-dir` — the same resolution
/// `horizon-agentd`'s `worktree::project_root` uses, so a linked or
/// isolated worktree resolves back to its source repository's main
/// toplevel. `None` when `dir` is not in a git repository or the path
/// cannot be canonicalized. The knowledge store is keyed by this (not
/// the worktree's own toplevel) so every worktree of one project shares
/// the same entries.
pub(crate) fn main_root(dir: &Path) -> Option<PathBuf> {
    let mut cmd = std::process::Command::new("git");
    cmd.arg("-C")
        .arg(dir)
        .args(["rev-parse", "--path-format=absolute", "--git-common-dir"]);
    scrub_git_env(&mut cmd);
    let output = cmd.output().ok()?;
    if !output.status.success() {
        return None;
    }
    let common_dir = PathBuf::from(String::from_utf8_lossy(&output.stdout).trim());
    let repo_root = common_dir.parent()?;
    std::fs::canonicalize(repo_root).ok()
}

// --- frontmatter ----------------------------------------------------------

/// An entry's publication status. Only `Active` entries appear in the
/// always-loaded prompt index; `knowledge.read` returns any status.
#[derive(Clone, Debug, Eq, PartialEq)]
enum KnowledgeStatus {
    Active,
    NeedsReview,
    Expired,
}

impl KnowledgeStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::NeedsReview => "needs-review",
            Self::Expired => "expired",
        }
    }
}

fn parse_status(value: &str) -> Option<KnowledgeStatus> {
    match value {
        "active" => Some(KnowledgeStatus::Active),
        "needs-review" => Some(KnowledgeStatus::NeedsReview),
        "expired" => Some(KnowledgeStatus::Expired),
        _ => None,
    }
}

/// A parsed knowledge entry — frontmatter fields plus the Markdown body.
/// `sources` is required (at least one); the parser rejects an entry
/// whose `sources` is empty or missing.
#[derive(Clone, Debug)]
struct ParsedKnowledgeMd {
    id: String,
    description: String,
    anchors: Vec<String>,
    sources: Vec<String>,
    created: String,
    updated: String,
    status: KnowledgeStatus,
    body: String,
}

/// Whether `id` is a valid slug: non-empty, lowercase ASCII alphanumeric
/// and hyphens only.
fn is_valid_slug(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Strips surrounding double or single quotes from `s`, if present.
fn strip_quotes(s: &str) -> String {
    let s = s.trim();
    if s.len() >= 2
        && ((s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')))
    {
        s[1..s.len() - 1].to_string()
    } else {
        s.to_string()
    }
}

/// Parses an inline YAML array `[item1, item2, ...]`, stripping
/// surrounding quotes from each item. Empty brackets yield an empty
/// vec.
fn parse_inline_array(value: &str) -> Vec<String> {
    let value = value.trim();
    let inner = value
        .strip_prefix('[')
        .and_then(|v| v.strip_suffix(']'))
        .unwrap_or(value);
    inner
        .split(',')
        .map(|item| strip_quotes(item.trim()))
        .filter(|item| !item.is_empty())
        .collect()
}

/// Hand-parses a knowledge entry's frontmatter (same `---`-delimited
/// shape as `skills::parse_skill_md`, extended with `anchors`/`sources`
/// arrays and `created`/`updated`/`status` fields). Returns `None` for
/// anything that doesn't match the expected shape (missing delimiters,
/// a required field missing, an unrecognized `status`, or an empty
/// `sources` list). Both inline arrays (`key: [a, b]`) and block arrays
/// (`key:\n  - a\n  - b`) are accepted.
fn parse_knowledge_md(source: &str) -> Option<ParsedKnowledgeMd> {
    let after_open = source.strip_prefix("---\n")?;
    let (frontmatter, body) = after_open.split_once("\n---\n")?;

    let mut id = None;
    let mut description = None;
    let mut anchors = Vec::new();
    let mut sources = Vec::new();
    let mut created = None;
    let mut updated = None;
    let mut status = None;

    enum CurrentArray {
        Anchors,
        Sources,
    }
    let mut current: Option<CurrentArray> = None;

    for line in frontmatter.lines() {
        // Block-array item: `  - value`
        if let Some(rest) = line.strip_prefix("  - ") {
            let item = strip_quotes(rest.trim());
            match &current {
                Some(CurrentArray::Anchors) => anchors.push(item),
                Some(CurrentArray::Sources) => sources.push(item),
                None => {}
            }
            continue;
        }

        current = None;

        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();

        match key {
            "id" => id = Some(value.to_string()),
            "description" => description = Some(value.to_string()),
            "created" => created = Some(value.to_string()),
            "updated" => updated = Some(value.to_string()),
            "status" => status = parse_status(value),
            "anchors" => {
                if value.is_empty() {
                    current = Some(CurrentArray::Anchors);
                } else {
                    anchors = parse_inline_array(value);
                }
            }
            "sources" => {
                if value.is_empty() {
                    current = Some(CurrentArray::Sources);
                } else {
                    sources = parse_inline_array(value);
                }
            }
            _ => {}
        }
    }

    // `sources` is required and must be non-empty.
    if sources.is_empty() {
        return None;
    }

    Some(ParsedKnowledgeMd {
        id: id?,
        description: description?,
        anchors,
        sources,
        created: created?,
        updated: updated?,
        status: status?,
        body: body.trim_start_matches('\n').to_string(),
    })
}

/// Serializes an entry back to the `---`-delimited file format. Used by
/// `knowledge.write` to persist an upserted entry.
fn serialize_entry(entry: &ParsedKnowledgeMd) -> String {
    fn fmt_array(items: &[String]) -> String {
        if items.is_empty() {
            return "[]".to_string();
        }
        let quoted: Vec<String> = items
            .iter()
            .map(|s| format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\"")))
            .collect();
        format!("[{}]", quoted.join(", "))
    }

    let mut out = String::new();
    out.push_str("---\n");
    out.push_str(&format!("id: {}\n", entry.id));
    out.push_str(&format!("description: {}\n", entry.description));
    out.push_str(&format!("anchors: {}\n", fmt_array(&entry.anchors)));
    out.push_str(&format!("sources: {}\n", fmt_array(&entry.sources)));
    out.push_str(&format!("created: {}\n", entry.created));
    out.push_str(&format!("updated: {}\n", entry.updated));
    out.push_str(&format!("status: {}\n", entry.status.as_str()));
    out.push_str("---\n");
    out.push_str(&entry.body);
    out
}

// --- date helper ----------------------------------------------------------

/// Today's date as `YYYY-MM-DD` in UTC. Computed from the Unix epoch
/// via the civil-date algorithm (Howard Hinnant's `civil_from_days`),
/// avoiding a `chrono`/`time` dependency — the date is for ordering
/// and display only, so UTC (not local time) is acceptable.
fn today() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let days = (secs / 86_400) as i64;
    // civil_from_days: days since 1970-01-01 → (year, month, day)
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// --- prompt index ---------------------------------------------------------

/// Discovers every parseable knowledge entry in `store_dir`. Unparsable
/// or unreadable files are warned and skipped (config-file-style
/// never-crash policy — mirrors `skills::discover_repository_skills`).
fn discover_entries(store_dir: &Path) -> Vec<ParsedKnowledgeMd> {
    let Ok(entries) = std::fs::read_dir(store_dir) else {
        return Vec::new();
    };
    let mut parsed = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_file() || path.extension().is_none_or(|ext| ext != "md") {
            continue;
        }
        match std::fs::read_to_string(&path) {
            Ok(source) => match parse_knowledge_md(&source) {
                Some(entry) => parsed.push(entry),
                None => tracing::warn!(
                    path = %path.display(),
                    "knowledge entry has unparsable frontmatter; skipping"
                ),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => tracing::warn!(
                path = %path.display(),
                error = %error,
                "failed to read knowledge entry; skipping"
            ),
        }
    }
    parsed
}

/// Builds the always-loaded prompt section listing `id: description`
/// for every `status: active` entry in the store keyed by `cwd`'s
/// project main root. Entries are adopted in `updated`-descending order
/// until the section would exceed `KNOWLEDGE_INDEX_CAP_CHARS`.
/// `None` when `cwd` is not in a git repo, the store directory has no
/// active entries, or the store doesn't exist yet.
pub(crate) fn prompt_section(cwd: &Path) -> Option<String> {
    let root = main_root(cwd)?;
    let store_dir = store_dir_for_root(&root);
    let mut active: Vec<ParsedKnowledgeMd> = discover_entries(&store_dir)
        .into_iter()
        .filter(|e| e.status == KnowledgeStatus::Active)
        .collect();
    if active.is_empty() {
        return None;
    }
    active.sort_by(|a, b| b.updated.cmp(&a.updated));

    let mut section = String::from(
        "Project knowledge distilled from past session logs; each entry cites verifiable \
         sources. Read a full entry with knowledge.read.\n",
    );
    let mut chars = section.chars().count();
    for entry in &active {
        let line = format!("- `{}`: {}\n", entry.id, entry.description);
        let line_chars = line.chars().count();
        if chars + line_chars > KNOWLEDGE_INDEX_CAP_CHARS {
            break;
        }
        section.push_str(&line);
        chars += line_chars;
    }
    Some(section.trim_end().to_string())
}

// --- tool handlers --------------------------------------------------------

use crate::tools::error_output;

/// Executes `knowledge.read`: returns one entry's frontmatter and body,
/// re-read from disk. Any status is readable. The entry is looked up by
/// `id` in the store keyed by `root` (the session's project main root,
/// resolved by the caller).
pub(crate) fn execute_read(root: &Path, input: &serde_json::Value) -> serde_json::Value {
    let Some(id) = input.get("id").and_then(serde_json::Value::as_str) else {
        return error_output("knowledge.read requires an `id` string argument");
    };
    if !is_valid_slug(id) {
        return error_output(format!(
            "knowledge.read: `id` must be a slug (lowercase alphanumeric and hyphens), got `{id}`"
        ));
    }
    let path = store_dir_for_root(root).join(format!("{id}.md"));
    match std::fs::read_to_string(&path) {
        Ok(source) => match parse_knowledge_md(&source) {
            Some(entry) => {
                let (body, truncated) = cap_to_chars(entry.body, KNOWLEDGE_BODY_CAP_CHARS);
                serde_json::json!({
                    "id": entry.id,
                    "description": entry.description,
                    "anchors": entry.anchors,
                    "sources": entry.sources,
                    "created": entry.created,
                    "updated": entry.updated,
                    "status": entry.status.as_str(),
                    "body": body,
                    "truncated": truncated,
                })
            }
            None => error_output(format!(
                "knowledge entry `{id}` at {} has unparsable frontmatter",
                path.display()
            )),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => error_output(format!(
            "no knowledge entry `{id}` — write one with knowledge.write"
        )),
        Err(error) => error_output(format!("cannot read `{}`: {error}", path.display())),
    }
}

/// Executes `knowledge.write`: upserts an entry. Validates `id` (slug),
/// `description` (non-empty), and `sources` (non-empty). An existing
/// entry's `created` date is preserved; `updated` is always set to
/// today. Optional fields (`anchors`, `status`) default to the existing
/// entry's values (or empty/active for a new entry). No approval — the
/// tool-event recording is the audit.
pub(crate) fn execute_write(root: &Path, input: &serde_json::Value) -> serde_json::Value {
    let Some(id) = input.get("id").and_then(serde_json::Value::as_str) else {
        return error_output("knowledge.write requires an `id` string argument");
    };
    if !is_valid_slug(id) {
        return error_output(format!(
            "knowledge.write: `id` must be a slug (lowercase alphanumeric and hyphens), got `{id}`"
        ));
    }
    let Some(description) = input.get("description").and_then(serde_json::Value::as_str) else {
        return error_output("knowledge.write requires a `description` string argument");
    };
    if description.trim().is_empty() {
        return error_output("knowledge.write: `description` must not be empty");
    }
    let Some(body) = input.get("body").and_then(serde_json::Value::as_str) else {
        return error_output("knowledge.write requires a `body` string argument");
    };
    let Some(sources) = input.get("sources").and_then(serde_json::Value::as_array) else {
        return error_output("knowledge.write requires a `sources` array argument");
    };
    let sources: Vec<String> = sources
        .iter()
        .filter_map(|s| s.as_str().map(|s| s.to_string()))
        .collect();
    if sources.is_empty() {
        return error_output("knowledge.write: `sources` must contain at least one entry");
    }

    let anchors = input
        .get("anchors")
        .and_then(serde_json::Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|s| s.as_str().map(|s| s.to_string()))
                .collect()
        });
    let status = input
        .get("status")
        .and_then(serde_json::Value::as_str)
        .and_then(parse_status);

    let path = store_dir_for_root(root).join(format!("{id}.md"));

    // Read existing entry to preserve `created` and any fields the
    // caller didn't supply.
    let existing = std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| parse_knowledge_md(&s));
    let created = existing
        .as_ref()
        .map(|e| e.created.clone())
        .unwrap_or_else(today);
    let anchors = anchors.unwrap_or_else(|| {
        existing
            .as_ref()
            .map(|e| e.anchors.clone())
            .unwrap_or_default()
    });
    let status = status.unwrap_or_else(|| {
        existing
            .as_ref()
            .map(|e| e.status.clone())
            .unwrap_or(KnowledgeStatus::Active)
    });
    let updated = today();

    let entry = ParsedKnowledgeMd {
        id: id.to_string(),
        description: description.to_string(),
        anchors,
        sources,
        created,
        updated,
        status,
        body: body.to_string(),
    };

    // Create the store directory if it doesn't exist yet (same as the
    // event-log writer's create-parent-on-first-write policy).
    if let Err(error) = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(""))) {
        return error_output(format!("cannot create knowledge store directory: {error}"));
    }

    let serialized = serialize_entry(&entry);
    match std::fs::write(&path, &serialized) {
        Ok(()) => serde_json::json!({
            "id": entry.id,
            "description": entry.description,
            "path": path.display().to_string(),
            "created": entry.created,
            "updated": entry.updated,
            "status": entry.status.as_str(),
        }),
        Err(error) => error_output(format!("cannot write `{}`: {error}", path.display())),
    }
}

// --- tests ----------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Creates a real (empty) git repo in a temp directory. Uses `git init`
    /// (not just a fake `.git` dir) so `main_root`'s `git rev-parse
    /// --git-common-dir` resolves to *this* directory, not an ancestor's
    /// repo — the sandbox may place `TMPDIR` inside a worktree, which would
    /// silently redirect every test at the real project's knowledge store.
    fn temp_git_repo(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "horizon-agent-knowledge-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut cmd = std::process::Command::new("git");
        cmd.arg("init").arg(&dir);
        scrub_git_env(&mut cmd);
        cmd.output().expect("git init must succeed");
        dir
    }

    /// Redirects `XDG_DATA_HOME` to a fresh temp directory so tests never
    /// touch the developer's real knowledge store. Safe with nextest (one
    /// test per process); each test gets its own data home with a unique
    /// UUID so concurrent runs never collide.
    fn temp_data_home() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "horizon-agent-knowledge-data-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        std::env::set_var("XDG_DATA_HOME", &dir);
        dir
    }

    fn write_entry(store_dir: &Path, id: &str, body: &str) {
        std::fs::create_dir_all(store_dir).unwrap();
        std::fs::write(store_dir.join(format!("{id}.md")), body).unwrap();
    }

    fn valid_frontmatter(id: &str, description: &str, sources: &[&str], body: &str) -> String {
        let sources_str = sources
            .iter()
            .map(|s| format!("\"{s}\""))
            .collect::<Vec<_>>()
            .join(", ");
        format!(
            "---\nid: {id}\ndescription: {description}\nsources: [{sources_str}]\ncreated: 2026-01-01\nupdated: 2026-01-01\nstatus: active\n---\n{body}"
        )
    }

    // --- parse_knowledge_md ---

    #[test]
    fn parses_a_well_formed_entry() {
        let source = valid_frontmatter(
            "my-entry",
            "A test entry",
            &["session:abc seq:1-5"],
            "Body.",
        );
        let parsed = parse_knowledge_md(&source).expect("well-formed entry must parse");
        assert_eq!(parsed.id, "my-entry");
        assert_eq!(parsed.description, "A test entry");
        assert_eq!(parsed.sources, vec!["session:abc seq:1-5".to_string()]);
        assert_eq!(parsed.created, "2026-01-01");
        assert_eq!(parsed.updated, "2026-01-01");
        assert_eq!(parsed.status, KnowledgeStatus::Active);
        assert_eq!(parsed.body, "Body.");
    }

    #[test]
    fn rejects_an_entry_missing_the_closing_delimiter() {
        let source = "---\nid: x\ndescription: y\nsources: [\"s\"]\n# Body\n";
        assert!(parse_knowledge_md(source).is_none());
    }

    #[test]
    fn rejects_an_entry_with_empty_sources() {
        let source = "---\nid: x\ndescription: y\nsources: []\ncreated: 2026-01-01\nupdated: 2026-01-01\nstatus: active\n---\nBody\n";
        assert!(parse_knowledge_md(source).is_none());
    }

    #[test]
    fn rejects_an_entry_with_missing_sources() {
        let source = "---\nid: x\ndescription: y\ncreated: 2026-01-01\nupdated: 2026-01-01\nstatus: active\n---\nBody\n";
        assert!(parse_knowledge_md(source).is_none());
    }

    #[test]
    fn rejects_an_entry_with_an_unrecognized_status() {
        let source = "---\nid: x\ndescription: y\nsources: [\"s\"]\ncreated: 2026-01-01\nupdated: 2026-01-01\nstatus: draft\n---\nBody\n";
        assert!(parse_knowledge_md(source).is_none());
    }

    #[test]
    fn parses_inline_arrays_with_multiple_items() {
        let source = "---\nid: x\ndescription: y\nanchors: [\"a\", \"b\", \"c\"]\nsources: [\"s1\", \"s2\"]\ncreated: 2026-01-01\nupdated: 2026-01-01\nstatus: needs-review\n---\nBody\n";
        let parsed = parse_knowledge_md(source).expect("must parse");
        assert_eq!(
            parsed.anchors,
            vec!["a".to_string(), "b".to_string(), "c".to_string()]
        );
        assert_eq!(parsed.sources, vec!["s1".to_string(), "s2".to_string()]);
        assert_eq!(parsed.status, KnowledgeStatus::NeedsReview);
    }

    #[test]
    fn parses_block_arrays() {
        let source = "---\nid: x\ndescription: y\nanchors:\n  - \"a\"\n  - \"b\"\nsources:\n  - \"s1\"\ncreated: 2026-01-01\nupdated: 2026-01-01\nstatus: expired\n---\nBody\n";
        let parsed = parse_knowledge_md(source).expect("must parse");
        assert_eq!(parsed.anchors, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(parsed.sources, vec!["s1".to_string()]);
        assert_eq!(parsed.status, KnowledgeStatus::Expired);
    }

    #[test]
    fn parses_unquoted_inline_arrays() {
        let source = "---\nid: x\ndescription: y\nanchors: [a, b]\nsources: [\"s1\"]\ncreated: 2026-01-01\nupdated: 2026-01-01\nstatus: active\n---\nBody\n";
        let parsed = parse_knowledge_md(source).expect("must parse");
        assert_eq!(parsed.anchors, vec!["a".to_string(), "b".to_string()]);
    }

    // --- is_valid_slug ---

    #[test]
    fn slug_accepts_lowercase_alnum_and_hyphens() {
        assert!(is_valid_slug("my-entry-123"));
        assert!(is_valid_slug("abc"));
        assert!(is_valid_slug("a-b-c"));
    }

    #[test]
    fn slug_rejects_uppercase_spaces_and_underscores() {
        assert!(!is_valid_slug("MyEntry"));
        assert!(!is_valid_slug("my entry"));
        assert!(!is_valid_slug("my_entry"));
        assert!(!is_valid_slug(""));
    }

    // --- serialize / round-trip ---

    #[test]
    fn serialize_round_trips_through_parse() {
        let entry = ParsedKnowledgeMd {
            id: "test".to_string(),
            description: "Test entry.".to_string(),
            anchors: vec!["path/to/file".to_string()],
            sources: vec!["session:abc seq:1-5".to_string()],
            created: "2026-01-01".to_string(),
            updated: "2026-01-02".to_string(),
            status: KnowledgeStatus::NeedsReview,
            body: "# Body\n\nText.".to_string(),
        };
        let serialized = serialize_entry(&entry);
        let parsed = parse_knowledge_md(&serialized).expect("serialized entry must re-parse");
        assert_eq!(parsed.id, entry.id);
        assert_eq!(parsed.description, entry.description);
        assert_eq!(parsed.anchors, entry.anchors);
        assert_eq!(parsed.sources, entry.sources);
        assert_eq!(parsed.created, entry.created);
        assert_eq!(parsed.updated, entry.updated);
        assert_eq!(parsed.status, entry.status);
        assert_eq!(parsed.body, entry.body);
    }

    #[test]
    fn serialize_handles_empty_anchors() {
        let entry = ParsedKnowledgeMd {
            id: "test".to_string(),
            description: "d".to_string(),
            anchors: Vec::new(),
            sources: vec!["s".to_string()],
            created: "2026-01-01".to_string(),
            updated: "2026-01-01".to_string(),
            status: KnowledgeStatus::Active,
            body: "b".to_string(),
        };
        let serialized = serialize_entry(&entry);
        assert!(serialized.contains("anchors: []"));
        let parsed = parse_knowledge_md(&serialized).expect("must re-parse");
        assert!(parsed.anchors.is_empty());
    }

    // --- prompt_section ---

    #[test]
    fn prompt_section_is_none_when_store_directory_does_not_exist() {
        let _data = temp_data_home();
        let root = temp_git_repo("no-store");
        // No knowledge store written — prompt_section should return None.
        assert_eq!(prompt_section(&root), None);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn prompt_section_lists_only_active_entries() {
        let _data = temp_data_home();
        let root = temp_git_repo("active-only");
        let store_dir = store_dir_for_root(&root);
        write_entry(
            &store_dir,
            "active-1",
            &valid_frontmatter("active-1", "Active entry", &["s1"], "body"),
        );
        write_entry(
            &store_dir,
            "expired-1",
            "---\nid: expired-1\ndescription: Expired entry\nsources: [\"s2\"]\ncreated: 2026-01-01\nupdated: 2026-01-01\nstatus: expired\n---\nbody\n",
        );

        let section = prompt_section(&root).expect("must build a section");
        assert!(section.contains("active-1"));
        assert!(section.contains("Active entry"));
        assert!(!section.contains("expired-1"));
        assert!(section.contains("knowledge.read"));
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn prompt_section_adopts_newer_entries_first_when_capped() {
        let _data = temp_data_home();
        let root = temp_git_repo("cap");
        let store_dir = store_dir_for_root(&root);
        // Write many entries with long descriptions to exceed the cap.
        for i in 0..50 {
            let updated = format!("2026-01-{:02}", i + 1);
            let body = format!(
                "---\nid: entry-{i:02}\ndescription: {} entry {i:02}\nsources: [\"s{i}\"]\ncreated: 2026-01-01\nupdated: {updated}\nstatus: active\n---\nbody\n",
                "x".repeat(400),
            );
            write_entry(&store_dir, &format!("entry-{i:02}"), &body);
        }

        let section = prompt_section(&root).expect("must build a section");
        assert!(
            section.chars().count() <= KNOWLEDGE_INDEX_CAP_CHARS,
            "section must not exceed the cap: {} chars",
            section.chars().count()
        );
        // The newest entry (updated 2026-01-50) should be present.
        assert!(
            section.contains("entry-49"),
            "newest entry must be adopted first: {section}"
        );
        // The oldest should be dropped (cap exceeded).
        assert!(
            !section.contains("entry-00"),
            "oldest entry should be dropped when the cap is exceeded"
        );
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn prompt_section_is_none_outside_a_git_repository() {
        let _data = temp_data_home();
        let dir = std::env::temp_dir().join(format!(
            "horizon-agent-knowledge-non-git-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Prevent git from walking up past this directory's parent — the
        // sandbox may place TMPDIR inside a worktree, which would make
        // `main_root` resolve to the real project instead of returning
        // None. `scrub_git_env` preserves `GIT_CEILING_DIRECTORIES`.
        std::env::set_var("GIT_CEILING_DIRECTORIES", dir.parent().unwrap());
        assert_eq!(prompt_section(&dir), None);
        std::env::remove_var("GIT_CEILING_DIRECTORIES");
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&_data);
    }

    // --- execute_read ---

    #[test]
    fn execute_read_returns_an_entry_for_a_known_id() {
        let _data = temp_data_home();
        let root = temp_git_repo("read");
        let store_dir = store_dir_for_root(&root);
        write_entry(
            &store_dir,
            "my-entry",
            &valid_frontmatter("my-entry", "Test entry", &["s"], "Body text."),
        );
        let output = execute_read(&root, &serde_json::json!({ "id": "my-entry" }));
        assert_eq!(output["id"], "my-entry");
        assert_eq!(output["body"], "Body text.");
        assert_eq!(output["truncated"], false);
        assert_eq!(output["status"], "active");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn execute_read_rereads_after_disk_edit() {
        let _data = temp_data_home();
        let root = temp_git_repo("read-reread");
        let store_dir = store_dir_for_root(&root);
        write_entry(
            &store_dir,
            "e",
            &valid_frontmatter("e", "d", &["s"], "original"),
        );
        let first = execute_read(&root, &serde_json::json!({ "id": "e" }));
        assert_eq!(first["body"], "original");

        write_entry(
            &store_dir,
            "e",
            &valid_frontmatter("e", "d", &["s"], "edited"),
        );
        let second = execute_read(&root, &serde_json::json!({ "id": "e" }));
        assert_eq!(second["body"], "edited");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn execute_read_errors_for_unknown_id() {
        let _data = temp_data_home();
        let root = temp_git_repo("read-unknown");
        let output = execute_read(&root, &serde_json::json!({ "id": "no-such-entry" }));
        assert_eq!(output["is_error"], true);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn execute_read_errors_on_missing_id_argument() {
        let _data = temp_data_home();
        let root = temp_git_repo("read-no-arg");
        let output = execute_read(&root, &serde_json::json!({}));
        assert_eq!(output["is_error"], true);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn execute_read_errors_on_invalid_slug() {
        let _data = temp_data_home();
        let root = temp_git_repo("read-bad-slug");
        let output = execute_read(&root, &serde_json::json!({ "id": "Bad Slug!" }));
        assert_eq!(output["is_error"], true);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn execute_read_returns_any_status() {
        let _data = temp_data_home();
        let root = temp_git_repo("read-expired");
        let store_dir = store_dir_for_root(&root);
        write_entry(
            &store_dir,
            "exp",
            "---\nid: exp\ndescription: d\nsources: [\"s\"]\ncreated: 2026-01-01\nupdated: 2026-01-01\nstatus: expired\n---\nbody",
        );
        let output = execute_read(&root, &serde_json::json!({ "id": "exp" }));
        assert_eq!(output["status"], "expired");
        assert_eq!(output["body"], "body");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    // --- execute_write ---

    #[test]
    fn execute_write_creates_a_new_entry() {
        let _data = temp_data_home();
        let root = temp_git_repo("write-new");
        let output = execute_write(
            &root,
            &serde_json::json!({
                "id": "new-entry",
                "description": "A new entry",
                "body": "Body text.",
                "sources": ["session:abc seq:1-5"],
            }),
        );
        assert_eq!(output["id"], "new-entry");
        assert_eq!(output["status"], "active");
        assert!(output["path"].as_str().unwrap().contains("new-entry.md"));

        // Round-trip: read it back.
        let read = execute_read(&root, &serde_json::json!({ "id": "new-entry" }));
        assert_eq!(read["description"], "A new entry");
        assert_eq!(read["body"], "Body text.");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn execute_write_upserts_an_existing_entry_preserving_created() {
        let _data = temp_data_home();
        let root = temp_git_repo("write-upsert");
        // Write the initial entry.
        let first = execute_write(
            &root,
            &serde_json::json!({
                "id": "e",
                "description": "original",
                "body": "original body",
                "sources": ["s1"],
            }),
        );
        let created = first["created"].as_str().unwrap().to_string();

        // Upsert with new content.
        let second = execute_write(
            &root,
            &serde_json::json!({
                "id": "e",
                "description": "updated desc",
                "body": "updated body",
                "sources": ["s1", "s2"],
            }),
        );
        assert_eq!(second["created"], created, "created must be preserved");
        assert_eq!(second["updated"], created, "updated must be today");

        // Verify the content was overwritten.
        let read = execute_read(&root, &serde_json::json!({ "id": "e" }));
        assert_eq!(read["description"], "updated desc");
        assert_eq!(read["body"], "updated body");
        assert_eq!(read["sources"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn execute_write_validates_id_slug() {
        let _data = temp_data_home();
        let root = temp_git_repo("write-bad-slug");
        let output = execute_write(
            &root,
            &serde_json::json!({
                "id": "Bad Slug",
                "description": "d",
                "body": "b",
                "sources": ["s"],
            }),
        );
        assert_eq!(output["is_error"], true);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn execute_write_validates_description_non_empty() {
        let _data = temp_data_home();
        let root = temp_git_repo("write-empty-desc");
        let output = execute_write(
            &root,
            &serde_json::json!({
                "id": "e",
                "description": "",
                "body": "b",
                "sources": ["s"],
            }),
        );
        assert_eq!(output["is_error"], true);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn execute_write_validates_sources_non_empty() {
        let _data = temp_data_home();
        let root = temp_git_repo("write-empty-sources");
        let output = execute_write(
            &root,
            &serde_json::json!({
                "id": "e",
                "description": "d",
                "body": "b",
                "sources": [],
            }),
        );
        assert_eq!(output["is_error"], true);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn execute_write_accepts_optional_anchors_and_status() {
        let _data = temp_data_home();
        let root = temp_git_repo("write-opts");
        let output = execute_write(
            &root,
            &serde_json::json!({
                "id": "e",
                "description": "d",
                "body": "b",
                "sources": ["s"],
                "anchors": ["path/to/file", "some/symbol"],
                "status": "needs-review",
            }),
        );
        assert_eq!(output["status"], "needs-review");

        let read = execute_read(&root, &serde_json::json!({ "id": "e" }));
        assert_eq!(read["status"], "needs-review");
        assert_eq!(read["anchors"].as_array().unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn execute_write_preserves_anchors_and_status_on_upsert_without_them() {
        let _data = temp_data_home();
        let root = temp_git_repo("write-preserve");
        execute_write(
            &root,
            &serde_json::json!({
                "id": "e",
                "description": "d",
                "body": "b",
                "sources": ["s"],
                "anchors": ["a1"],
                "status": "needs-review",
            }),
        );
        // Upsert without anchors/status — they should be preserved.
        execute_write(
            &root,
            &serde_json::json!({
                "id": "e",
                "description": "d2",
                "body": "b2",
                "sources": ["s"],
            }),
        );
        let read = execute_read(&root, &serde_json::json!({ "id": "e" }));
        assert_eq!(read["anchors"].as_array().unwrap(), &["a1"]);
        assert_eq!(read["status"], "needs-review");
        assert_eq!(read["description"], "d2");
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }

    #[test]
    fn execute_write_caps_oversized_body() {
        let _data = temp_data_home();
        let root = temp_git_repo("write-cap");
        let big_body = "x".repeat(KNOWLEDGE_BODY_CAP_CHARS + 1_000);
        execute_write(
            &root,
            &serde_json::json!({
                "id": "e",
                "description": "d",
                "body": &big_body,
                "sources": ["s"],
            }),
        );
        let read = execute_read(&root, &serde_json::json!({ "id": "e" }));
        assert_eq!(read["truncated"], true);
        assert!(read["body"].as_str().unwrap().chars().count() <= KNOWLEDGE_BODY_CAP_CHARS);
        let _ = std::fs::remove_dir_all(&root);
        let _ = std::fs::remove_dir_all(&_data);
    }
}
