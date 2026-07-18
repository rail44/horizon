//! A minimal skill mechanism: progressive disclosure per
//! `docs/research/agent-prompting.md` Part 3.2's design-material sketch.
//!
//! **v1** (`docs/agent-roles-and-skills-design.md`) shipped skills embedded
//! in the binary only (`include_str!`), advertised only to sessions whose
//! role named them. **v2** (dated section in the same doc) adds a second,
//! per-session layer: `.horizon/skills/<id>/SKILL.md` directories discovered
//! by walking from the session's cwd up to its git root (same walk
//! discipline as `instructions::extra_sections` — see
//! [`ancestor_dirs_from_git_root`]), which win over an embedded skill
//! sharing the same id, and are advertised to *every* session (role-less
//! sessions now get a skills section too, listing every available skill —
//! see [`SkillRegistry::prompt_section_for_all`]).
//!
//! **Trust note.** A repository skill is exactly the prompt-injection
//! surface `docs/trust-boundaries.md`'s tier reasoning warns about — the
//! same surface `roles::CONFIG_ROLE`'s doc comment already flags for
//! `AGENTS.md`/`CLAUDE.md` ingestion — and arguably sharper here, since a
//! repository skill *overrides* an embedded one by id (any session working
//! in a repo could have `horizon-config`'s instructions silently shadowed).
//! Accepted anyway (owner decision, 2026-07-07): Horizon is presently a
//! personal project, and this is a deliberate hypothesis-testing setup that
//! lets a skill be iterated on without rebuilding the binary — exactly the
//! same trade repository-instructions ingestion already makes.
//!
//! Three stages of disclosure, matching the design material:
//!
//! 1. **Always loaded**: one extra prompt section listing every available
//!    skill (name + description) this session can see — every skill for a
//!    role-less session ([`SkillRegistry::prompt_section_for_all`]), or just
//!    a role's `skill_ids` for a role-bearing one
//!    ([`SkillRegistry::prompt_section_for_ids`]).
//! 2. **Read on demand**: the `skill.read` tool ([`execute_read`]) returns
//!    one skill's full body, re-read from disk at read time for a
//!    repository skill (see [`Skill::body`]) so an edit followed by a fresh
//!    `skill.read` in the same session sees the new content.
//! 3. **Deeper resources**: not built -- no skill here references further
//!    files/scripts yet (Part 3.2's third disclosure tier). Add it if a
//!    future skill needs it.
//!
//! `SKILL.md` format (hand-parsed below, no new dependency): a YAML-ish
//! frontmatter block delimited by `---` lines, with `name:`/`description:`
//! keys, followed by the Markdown body. Identical for embedded and
//! repository skills.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::instructions::{ancestor_dirs_from_git_root, cap_to_chars};

/// Where a skill's body actually lives -- cached in the binary for an
/// embedded skill, or re-read from disk on every `skill.read` for a
/// repository skill (the "edit -> observe" loop the repository layer exists
/// for; see the module doc's trust note on why this is accepted).
#[derive(Clone, Debug)]
enum SkillBody {
    Embedded(String),
    Repository(PathBuf),
}

/// One skill, known either at compile time (embedded) or discovered from a
/// repository's `.horizon/skills/` directory for this session
/// ([`SkillRegistry::discover`]).
#[derive(Clone, Debug)]
pub struct Skill {
    /// The identifier used both in a role's `skill_ids` and in
    /// `skill.read`'s `id` input -- taken verbatim from the `SKILL.md`
    /// frontmatter's `name:` field (not the containing directory's name,
    /// for a repository skill).
    pub(crate) name: String,
    /// The one-line summary shown in the always-loaded prompt section --
    /// taken from `description:`.
    pub(crate) description: String,
    source: SkillBody,
}

impl Skill {
    /// This skill's full Markdown body, returned by `skill.read`. Cached
    /// for an embedded skill; re-read and re-parsed from disk for a
    /// repository skill. A repository skill whose file has since been
    /// deleted or no longer parses returns an explanatory placeholder
    /// rather than panicking or silently returning stale content.
    fn body(&self) -> String {
        match &self.source {
            SkillBody::Embedded(body) => body.clone(),
            SkillBody::Repository(path) => match std::fs::read_to_string(path)
                .ok()
                .and_then(|source| parse_skill_md(&source))
            {
                Some(parsed) => parsed.body,
                None => format!(
                    "(could not re-read skill body from {} -- it may have been deleted or its \
                     frontmatter no longer parses)",
                    path.display()
                ),
            },
        }
    }
}

/// This build's embedded skills, ship-native so the configuration agent
/// (and `horizon-cli`'s generic-session audience) work no matter where
/// Horizon was launched or whether any repository skills exist at all.
const HORIZON_CONFIG_SKILL_SOURCE: &str = include_str!("../skills/horizon-config/SKILL.md");
const HORIZON_CLI_SKILL_SOURCE: &str = include_str!("../skills/horizon-cli/SKILL.md");
const HORIZON_DISTILL_SKILL_SOURCE: &str = include_str!("../skills/horizon-distill/SKILL.md");

/// Every skill this build embeds, parsed once and cached for the process's
/// lifetime -- the input [`SkillRegistry::discover`] starts every session's
/// composition from.
fn embedded_skills() -> &'static [Skill] {
    static SKILLS: std::sync::OnceLock<Vec<Skill>> = std::sync::OnceLock::new();
    SKILLS.get_or_init(|| {
        [
            HORIZON_CONFIG_SKILL_SOURCE,
            HORIZON_CLI_SKILL_SOURCE,
            HORIZON_DISTILL_SKILL_SOURCE,
        ]
        .into_iter()
        .map(|source| {
            let parsed =
                parse_skill_md(source).expect("crates/horizon-agent/skills/*/SKILL.md must parse");
            Skill {
                name: parsed.name,
                description: parsed.description,
                source: SkillBody::Embedded(parsed.body),
            }
        })
        .collect()
    })
}

/// Bounds a skill's body as returned by `skill.read` -- a repository skill
/// is arbitrary user-controlled content (unlike an embedded one, sized by
/// this crate's own authors), so it needs the same size-cap discipline
/// `config::RigAgentConfig::repository_instructions_cap_chars` applies to
/// repository instruction files. A plain constant (not a config knob):
/// there's no per-deployment reason this would need tuning, and the crate
/// can't read Horizon's config file itself (see `config`'s module doc).
const SKILL_BODY_CAP_CHARS: usize = 24_000;

/// Per-session skill registry: this build's embedded skills, overridden by
/// any repository skill sharing the same id (see the module doc's trust
/// note), sorted by id for deterministic listing order. Composed once per
/// session (not a global static) because repository discovery is
/// cwd-dependent -- see `providers::rig::session::session_extra_sections`
/// and `horizon-sessiond`'s `session::run_session`, the two production sites
/// that build one via [`Self::discover`].
#[derive(Default)]
pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    /// Builds the registry for a session whose working directory is `cwd`:
    /// every embedded skill, with each `.horizon/skills/<id>/SKILL.md`
    /// discovered while walking from the repository root (or just `cwd`
    /// outside a git repository) down to `cwd` inserted last, so a
    /// repository skill always overrides an embedded one sharing its id
    /// (and a `cwd`-nearer repository skill overrides a root-level one --
    /// mirroring `instructions::extra_sections`' "nested refines root"
    /// order).
    pub fn discover(cwd: &Path) -> Self {
        let mut by_id: HashMap<String, Skill> = embedded_skills()
            .iter()
            .map(|skill| (skill.name.clone(), skill.clone()))
            .collect();
        for skill in discover_repository_skills(cwd) {
            by_id.insert(skill.name.clone(), skill);
        }
        let mut skills: Vec<Skill> = by_id.into_values().collect();
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        Self { skills }
    }

    /// Looks up a skill by id in this session's composed registry.
    pub(crate) fn get(&self, id: &str) -> Option<&Skill> {
        self.skills.iter().find(|skill| skill.name == id)
    }

    /// Every id currently known to this registry, in listing order --
    /// used by [`execute_read`] to report available ids for an unknown one.
    fn ids(&self) -> Vec<&str> {
        self.skills
            .iter()
            .map(|skill| skill.name.as_str())
            .collect()
    }

    /// Builds the always-loaded prompt section listing *every* skill in
    /// this registry -- the role-less/generic-session case added in v2.
    /// `None` for an empty registry, so a hypothetical build shipping no
    /// skills at all reproduces the pre-skill prompt byte-for-byte (this
    /// build always embeds at least `horizon-config`/`horizon-cli`, so this
    /// is exercised directly against an empty registry in this module's own
    /// tests rather than through [`Self::discover`]).
    pub(crate) fn prompt_section_for_all(&self) -> Option<String> {
        skills_prompt_section(self.skills.iter())
    }

    /// Builds the always-loaded prompt section restricted to `skill_ids`
    /// (a role's `roles::RoleDefinition::skill_ids`), in the given order,
    /// silently skipping any id this registry doesn't resolve (e.g. stale
    /// data from a build that no longer ships that skill). `None` for an
    /// empty slice.
    pub(crate) fn prompt_section_for_ids(&self, skill_ids: &[&str]) -> Option<String> {
        if skill_ids.is_empty() {
            return None;
        }
        skills_prompt_section(skill_ids.iter().filter_map(|id| self.get(id)))
    }
}

/// Shared body for [`SkillRegistry::prompt_section_for_all`]/
/// [`SkillRegistry::prompt_section_for_ids`]: "`<name>` -- `<description>`"
/// per skill plus one line telling the agent to read a skill via
/// `skill.read` before relying on it -- `docs/research/agent-prompting.md`
/// Part 3.2's "metadata now, body on demand" shape. `None` if `skills`
/// yields nothing, so a session with nothing to disclose adds no extra
/// section at all.
fn skills_prompt_section<'a>(skills: impl Iterator<Item = &'a Skill>) -> Option<String> {
    let mut section = String::from(
        "Skills available for this session -- read one with `skill.read` before relying on \
         its contents:\n",
    );
    let mut any = false;
    for skill in skills {
        any = true;
        section.push_str(&format!("- `{}` -- {}\n", skill.name, skill.description));
    }
    any.then(|| section.trim_end().to_string())
}

/// Discovers repository skills for `cwd`: every `.horizon/skills/<id>/SKILL.md`
/// found while walking from the repository root (or just `cwd`, outside a
/// git repository) down to `cwd` -- see [`ancestor_dirs_from_git_root`].
/// Reads and parses each `SKILL.md` in full at discovery time (there's no
/// cheaper way to reach just the frontmatter with this crate's hand-rolled
/// parser), but only the parsed `name`/`description` are kept here -- the
/// body is re-read fresh from disk on every `skill.read` (see
/// [`Skill::body`]), not cached from this pass. A directory whose
/// `SKILL.md` doesn't parse warns and is skipped rather than failing the
/// whole session (config-file-style never-crash policy -- mirrors
/// `instructions::read_to_string_or_warn`).
fn discover_repository_skills(cwd: &Path) -> Vec<Skill> {
    let mut skills = Vec::new();
    for dir in ancestor_dirs_from_git_root(cwd) {
        let skills_dir = dir.join(".horizon").join("skills");
        let Ok(entries) = std::fs::read_dir(&skills_dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let skill_dir = entry.path();
            if !skill_dir.is_dir() {
                continue;
            }
            let skill_md = skill_dir.join("SKILL.md");
            match std::fs::read_to_string(&skill_md) {
                Ok(source) => match parse_skill_md(&source) {
                    Some(parsed) => skills.push(Skill {
                        name: parsed.name,
                        description: parsed.description,
                        source: SkillBody::Repository(skill_md),
                    }),
                    None => tracing::warn!(
                        path = %skill_md.display(),
                        "repository skill has unparsable frontmatter; skipping"
                    ),
                },
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => tracing::warn!(
                    path = %skill_md.display(),
                    error = %error,
                    "failed to read repository skill; skipping"
                ),
            }
        }
    }
    skills
}

/// A `SKILL.md`'s parsed frontmatter + body, before it's wrapped in a
/// [`Skill`] (which additionally needs to know whether the body is embedded
/// or re-read from `path` -- see [`SkillBody`]).
struct ParsedSkillMd {
    name: String,
    description: String,
    body: String,
}

/// Hand-parses a `SKILL.md`'s frontmatter (`---\nname: ..\ndescription: ..\n---\n<body>`)
/// with no YAML dependency -- the format is small enough that a real
/// parser would be over-engineering. Returns `None` for anything that
/// doesn't match the expected shape (missing delimiters, or a frontmatter
/// missing `name`/`description`); [`embedded_skills`] `.expect()`s this to
/// succeed for skills this crate ships itself (covered by this module's own
/// test), while [`discover_repository_skills`] treats a `None` here as
/// "skip and warn" for arbitrary repository content.
fn parse_skill_md(source: &str) -> Option<ParsedSkillMd> {
    let after_open = source.strip_prefix("---\n")?;
    let (frontmatter, body) = after_open.split_once("\n---\n")?;

    let mut name = None;
    let mut description = None;
    for line in frontmatter.lines() {
        let (key, value) = line.split_once(':')?;
        match key.trim() {
            "name" => name = Some(value.trim().to_string()),
            "description" => description = Some(value.trim().to_string()),
            _ => {}
        }
    }

    Some(ParsedSkillMd {
        name: name?,
        description: description?,
        body: body.trim_start_matches('\n').to_string(),
    })
}

/// Executes the `skill.read` tool against `registry` (this session's
/// composed [`SkillRegistry`]): returns `id`'s full body (capped at
/// [`SKILL_BODY_CAP_CHARS`]), or an error listing every id this session can
/// see if `id` doesn't match one. Takes the raw tool input rather than an
/// already-extracted `&str` so `tools::config` (the one caller) can
/// dispatch to this without its own argument-shape checking.
pub(crate) fn execute_read(
    registry: &SkillRegistry,
    input: &serde_json::Value,
) -> serde_json::Value {
    let Some(id) = input.get("id").and_then(serde_json::Value::as_str) else {
        return error_output("skill.read requires an `id` string argument");
    };
    match registry.get(id) {
        Some(skill) => {
            let (body, truncated) = cap_to_chars(skill.body(), SKILL_BODY_CAP_CHARS);
            serde_json::json!({
                "id": skill.name,
                "description": skill.description,
                "body": body,
                "truncated": truncated,
            })
        }
        None => error_output(format!(
            "unknown skill `{id}` -- available: {}",
            registry.ids().join(", ")
        )),
    }
}

fn error_output(message: impl Into<String>) -> serde_json::Value {
    serde_json::json!({ "is_error": true, "message": message.into() })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_repo(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "horizon-agent-skills-{label}-{}",
            uuid::Uuid::new_v4()
        ));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        dir
    }

    fn write_skill(dir: &Path, rel: &str, id: &str, description: &str, body: &str) {
        let skill_dir = dir.join(".horizon").join("skills").join(rel);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {id}\ndescription: {description}\n---\n{body}"),
        )
        .unwrap();
    }

    #[test]
    fn parses_a_well_formed_skill_md() {
        let source = "---\nname: example\ndescription: An example skill.\n---\n# Body\n\nHello.\n";
        let parsed = parse_skill_md(source).expect("well-formed frontmatter must parse");
        assert_eq!(parsed.name, "example");
        assert_eq!(parsed.description, "An example skill.");
        assert_eq!(parsed.body, "# Body\n\nHello.\n");
    }

    #[test]
    fn rejects_a_skill_md_missing_the_closing_delimiter() {
        let source = "---\nname: example\ndescription: broken\n# Body\n";
        assert!(parse_skill_md(source).is_none());
    }

    #[test]
    fn rejects_a_skill_md_missing_a_required_frontmatter_key() {
        let source = "---\nname: example\n---\nBody with no description.\n";
        assert!(parse_skill_md(source).is_none());
    }

    #[test]
    fn embedded_skills_parse_and_are_registered() {
        let registry = SkillRegistry::discover(&std::env::temp_dir());
        let config_skill = registry
            .get("horizon-config")
            .expect("horizon-config must be registered");
        assert!(!config_skill.description.is_empty());
        assert!(config_skill.body().contains("config.toml"));

        let cli_skill = registry
            .get("horizon-cli")
            .expect("horizon-cli must be registered");
        assert!(!cli_skill.description.is_empty());
        assert!(cli_skill.body().contains("horizon"));

        let distill_skill = registry
            .get("horizon-distill")
            .expect("horizon-distill must be registered");
        assert!(!distill_skill.description.is_empty());
        assert!(distill_skill.body().contains("recall.search"));
    }

    #[test]
    fn get_returns_none_for_an_unknown_skill_id() {
        let registry = SkillRegistry::default();
        assert!(registry.get("does-not-exist").is_none());
    }

    #[test]
    fn prompt_section_for_all_is_none_for_an_empty_registry() {
        let registry = SkillRegistry::default();
        assert_eq!(registry.prompt_section_for_all(), None);
    }

    #[test]
    fn prompt_section_for_ids_is_none_for_an_empty_id_list() {
        let registry = SkillRegistry::discover(&std::env::temp_dir());
        assert_eq!(registry.prompt_section_for_ids(&[]), None);
    }

    #[test]
    fn prompt_section_for_all_lists_every_skill_and_mentions_skill_read() {
        let registry = SkillRegistry::discover(&std::env::temp_dir());
        let section = registry
            .prompt_section_for_all()
            .expect("must build a section");
        assert!(section.contains("horizon-config"));
        assert!(section.contains("horizon-cli"));
        assert!(section.contains("skill.read"));
    }

    #[test]
    fn prompt_section_for_ids_lists_only_the_given_ids() {
        let registry = SkillRegistry::discover(&std::env::temp_dir());
        let section = registry
            .prompt_section_for_ids(&["horizon-config"])
            .expect("must build a section");
        assert!(section.contains("horizon-config"));
        assert!(!section.contains("horizon-cli"));
    }

    #[test]
    fn prompt_section_for_ids_silently_skips_an_unknown_skill_id() {
        let registry = SkillRegistry::discover(&std::env::temp_dir());
        let section = registry
            .prompt_section_for_ids(&["horizon-config", "does-not-exist"])
            .expect("must still build a section for the known entry");
        assert!(section.contains("horizon-config"));
        assert!(!section.contains("does-not-exist"));
    }

    #[test]
    fn discover_finds_a_repository_skill_at_the_repo_root() {
        let root = temp_repo("root-level");
        write_skill(&root, "my-skill", "my-skill", "A repo skill.", "Body text.");

        let registry = SkillRegistry::discover(&root);
        let skill = registry
            .get("my-skill")
            .expect("repo skill must be discovered");
        assert_eq!(skill.description, "A repo skill.");
        assert_eq!(skill.body(), "Body text.");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_finds_a_repository_skill_in_a_nested_ancestor() {
        let root = temp_repo("nested");
        let nested = root.join("crates").join("inner");
        std::fs::create_dir_all(&nested).unwrap();
        write_skill(&root, "my-skill", "my-skill", "A repo skill.", "Body text.");

        let registry = SkillRegistry::discover(&nested);
        assert!(registry.get("my-skill").is_some());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_skips_a_skill_dir_with_unparsable_frontmatter() {
        let root = temp_repo("bad-frontmatter");
        let skill_dir = root.join(".horizon").join("skills").join("broken");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "not a valid SKILL.md at all").unwrap();

        let registry = SkillRegistry::discover(&root);
        assert!(registry.get("broken").is_none());
        // The build's embedded skills must still be present -- one bad
        // repository skill must not take down the whole registry.
        assert!(registry.get("horizon-config").is_some());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_does_not_search_ancestors_outside_a_git_repository() {
        let root = std::env::temp_dir().join(format!(
            "horizon-agent-skills-non-git-{}",
            uuid::Uuid::new_v4()
        ));
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        write_skill(
            &root,
            "parent-only",
            "parent-only",
            "Should not appear.",
            "x",
        );

        let registry = SkillRegistry::discover(&nested);
        assert!(registry.get("parent-only").is_none());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_lets_a_repository_skill_override_an_embedded_one() {
        let root = temp_repo("override");
        write_skill(
            &root,
            "horizon-config",
            "horizon-config",
            "Overridden description.",
            "Overridden body.",
        );

        let registry = SkillRegistry::discover(&root);
        let skill = registry.get("horizon-config").unwrap();
        assert_eq!(skill.description, "Overridden description.");
        assert_eq!(skill.body(), "Overridden body.");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn discover_lets_a_nested_repository_skill_override_a_root_one_with_the_same_id() {
        let root = temp_repo("nested-override");
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        write_skill(&root, "dup", "dup", "root version", "root body");
        write_skill(&nested, "dup", "dup", "nested version", "nested body");

        let registry = SkillRegistry::discover(&nested);
        let skill = registry.get("dup").unwrap();
        assert_eq!(skill.description, "nested version");

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn execute_read_returns_the_body_for_a_known_id() {
        let registry = SkillRegistry::discover(&std::env::temp_dir());
        let output = execute_read(&registry, &serde_json::json!({ "id": "horizon-config" }));
        assert_eq!(output["id"], "horizon-config");
        assert!(output["body"].as_str().unwrap().contains("config.toml"));
        assert_eq!(output["truncated"], false);
    }

    #[test]
    fn execute_read_rereads_a_repository_skill_body_after_it_changes_on_disk() {
        let root = temp_repo("fresh-reread");
        write_skill(&root, "my-skill", "my-skill", "desc", "original body");
        let registry = SkillRegistry::discover(&root);

        let first = execute_read(&registry, &serde_json::json!({ "id": "my-skill" }));
        assert_eq!(first["body"], "original body");

        write_skill(&root, "my-skill", "my-skill", "desc", "edited body");
        let second = execute_read(&registry, &serde_json::json!({ "id": "my-skill" }));
        assert_eq!(
            second["body"], "edited body",
            "skill.read must re-read a repository skill's body from disk, not a cached copy"
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn execute_read_lists_available_ids_for_an_unknown_id() {
        let registry = SkillRegistry::discover(&std::env::temp_dir());
        let output = execute_read(&registry, &serde_json::json!({ "id": "no-such-skill" }));
        assert_eq!(output["is_error"], true);
        assert!(output["message"]
            .as_str()
            .unwrap()
            .contains("horizon-config"));
    }

    #[test]
    fn execute_read_errors_on_a_missing_id_argument() {
        let registry = SkillRegistry::discover(&std::env::temp_dir());
        let output = execute_read(&registry, &serde_json::json!({}));
        assert_eq!(output["is_error"], true);
    }

    #[test]
    fn execute_read_caps_an_oversized_repository_skill_body() {
        let root = temp_repo("oversized");
        let big_body = "x".repeat(SKILL_BODY_CAP_CHARS + 1_000);
        write_skill(&root, "big", "big", "desc", &big_body);
        let registry = SkillRegistry::discover(&root);

        let output = execute_read(&registry, &serde_json::json!({ "id": "big" }));
        assert_eq!(output["truncated"], true);
        assert!(output["body"].as_str().unwrap().chars().count() <= SKILL_BODY_CAP_CHARS);

        let _ = std::fs::remove_dir_all(&root);
    }
}
