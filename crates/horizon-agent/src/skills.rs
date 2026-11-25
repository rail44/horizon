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
//! same surface repository `AGENTS.md`/`CLAUDE.md` ingestion opens — and
//! arguably sharper here, since a
//! repository skill *overrides* an embedded one by id (any session working
//! in a repo could have `horizon-cli`'s instructions silently shadowed).
//! Originally accepted unconditionally as a personal-project
//! hypothesis-testing convenience; now gated behind a user-level trust
//! decision: a session whose
//! project root is not in the user's `trusted_projects` config list gets
//! [`SkillRegistry::embedded`] only — no repository skills are discovered
//! or loaded, and the same gate suppresses `AGENTS.md`/`CLAUDE.md`
//! instructions (`instructions::extra_sections`). Behavioral defense
//! (sandbox, approval) is unaffected: an untrusted session still runs.
//!
//! Three stages of disclosure, matching the design material:
//!
//! 1. **Always loaded**: one extra prompt section listing every available
//!    skill (name + description) this session can see — every skill for a
//!    role-less session ([`SkillRegistry::prompt_section_for_all`]), or just
//!    a role's `skill_ids` for a role-bearing one
//!    ([`SkillRegistry::prompt_section_for_ids`]).
//! 2. **Read on demand**: the `skill_read` tool ([`execute_read`]) returns
//!    one skill's full body, re-read from disk at read time for a
//!    repository skill (see [`Skill::body`]) so an edit followed by a fresh
//!    `skill_read` in the same session sees the new content.
//! 3. **Deeper resources**: a repository skill's `skill_read` also names
//!    its directory, so a file its instructions refer to by a relative path
//!    is read from there with `fs_read` (Part 3.2's third disclosure tier).
//!    A skill built into Horizon has no files beside its body.
//!
//! A repository keeps its skills in `.horizon/skills/<id>/SKILL.md` or, as
//! the Agent Skills convention shared with other agents has it,
//! `.agents/skills/<id>/SKILL.md`; where both have the same id at one
//! level, `.horizon/skills` wins.
//!
//! `SKILL.md` format, per the Agent Skills spec: a YAML frontmatter block
//! delimited by `---` lines with at least `name` and `description`,
//! followed by the Markdown body. Identical for embedded and repository
//! skills.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::instructions::{ancestor_dirs_from_git_root, cap_to_chars};

/// Where a skill's body actually lives -- cached in the binary for an
/// embedded skill, or re-read from disk on every `skill_read` for a
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
pub(crate) struct Skill {
    /// The identifier used both in a role's `skill_ids` and in
    /// `skill_read`'s `id` input -- taken verbatim from the `SKILL.md`
    /// frontmatter's `name:` field (not the containing directory's name,
    /// for a repository skill).
    pub(crate) name: String,
    /// The one-line summary shown in the always-loaded prompt section --
    /// taken from `description:`.
    pub(crate) description: String,
    source: SkillBody,
}

impl Skill {
    /// This skill's full Markdown body, returned by `skill_read`. Cached
    /// for an embedded skill; re-read and re-parsed from disk for a
    /// repository skill. A repository skill whose file has since been
    /// deleted or no longer parses returns an explanatory placeholder
    /// rather than panicking or silently returning stale content.
    /// Where a repository skill's files are; `None` for an embedded skill.
    fn directory(&self) -> Option<&Path> {
        match &self.source {
            SkillBody::Embedded(_) => None,
            SkillBody::Repository(path) => path.parent(),
        }
    }

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
const HORIZON_CLI_SKILL_SOURCE: &str = include_str!("../skills/horizon-cli/SKILL.md");
const HORIZON_DISTILL_SKILL_SOURCE: &str = include_str!("../skills/horizon-distill/SKILL.md");
const GITHUB_PR_SKILL_SOURCE: &str = include_str!("../skills/github-pr/SKILL.md");

/// Every skill this build embeds, parsed once and cached for the process's
/// lifetime. The skill sources packages add come on top of these
/// (`shipped_skills`).
fn embedded_skills() -> &'static [Skill] {
    static SKILLS: std::sync::OnceLock<Vec<Skill>> = std::sync::OnceLock::new();
    SKILLS.get_or_init(|| {
        let sources: Vec<&'static str> = vec![
            HORIZON_CLI_SKILL_SOURCE,
            HORIZON_DISTILL_SKILL_SOURCE,
            GITHUB_PR_SKILL_SOURCE,
        ];
        sources.into_iter().map(embedded_skill).collect()
    })
}

fn embedded_skill(source: &'static str) -> Skill {
    let parsed = parse_skill_md(source).expect("an embedded SKILL.md must parse");
    Skill {
        name: parsed.name,
        description: parsed.description,
        source: SkillBody::Embedded(parsed.body),
    }
}

/// This build's embedded skills, then the ones `packages` add.
fn shipped_skills(packages: &crate::packages::Packages) -> Vec<Skill> {
    let mut skills = embedded_skills().to_vec();
    skills.extend(packages.skill_sources().iter().copied().map(embedded_skill));
    skills
}

/// Bounds a skill's body as returned by `skill_read` -- a repository skill
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
/// and `horizon-agent-host`'s `session::run_session`, the two production sites
/// that build one via [`Self::discover`].
#[derive(Default)]
pub struct SkillRegistry {
    skills: Vec<Skill>,
}

impl SkillRegistry {
    /// Builds the registry from this build's embedded skills only — no
    /// `.horizon/skills/` repository discovery. Used for sessions whose
    /// project root is not in the user's `trusted_projects` list (see the
    /// module doc's trust note): embedded skills are ship-native, not
    /// repo-controlled, so they are always safe to advertise regardless of
    /// trust. Produces the same sorted set [`Self::discover`] starts from
    /// before overlaying repository skills.
    pub fn embedded(packages: &crate::packages::Packages) -> Self {
        let mut skills: Vec<Skill> = shipped_skills(packages);
        skills.sort_by(|a, b| a.name.cmp(&b.name));
        Self { skills }
    }

    /// Builds the registry for a session whose working directory is `cwd`:
    /// every embedded skill, with each `.horizon/skills/<id>/SKILL.md`
    /// discovered while walking from the repository root (or just `cwd`
    /// outside a git repository) down to `cwd` inserted last, so a
    /// repository skill always overrides an embedded one sharing its id
    /// (and a `cwd`-nearer repository skill overrides a root-level one --
    /// mirroring `instructions::extra_sections`' "nested refines root"
    /// order).
    pub fn discover(cwd: &Path, packages: &crate::packages::Packages) -> Self {
        let mut by_id: HashMap<String, Skill> = shipped_skills(packages)
            .into_iter()
            .map(|skill| (skill.name.clone(), skill))
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
    /// build always embeds at least `horizon-cli`/`horizon-cli`, so this
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
/// `skill_read` before relying on it -- `docs/research/agent-prompting.md`
/// Part 3.2's "metadata now, body on demand" shape. `None` if `skills`
/// yields nothing, so a session with nothing to disclose adds no extra
/// section at all.
fn skills_prompt_section<'a>(skills: impl Iterator<Item = &'a Skill>) -> Option<String> {
    let mut section = String::from(
        "Skills available for this session -- read one with `skill_read` before relying on \
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
/// body is re-read fresh from disk on every `skill_read` (see
/// [`Skill::body`]), not cached from this pass. A directory whose
/// `SKILL.md` doesn't parse warns and is skipped rather than failing the
/// whole session (config-file-style never-crash policy -- mirrors
/// `instructions::read_to_string_or_warn`).
fn discover_repository_skills(cwd: &Path) -> Vec<Skill> {
    let mut skills = Vec::new();
    // Later wins: at one level `.horizon/skills` over `.agents/skills`.
    let skill_dirs = ancestor_dirs_from_git_root(cwd)
        .into_iter()
        .flat_map(|dir| {
            [
                dir.join(".agents").join("skills"),
                dir.join(".horizon").join("skills"),
            ]
        });
    for skills_dir in skill_dirs {
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

/// The frontmatter fields a skill is listed by. The Agent Skills spec's
/// other fields (`license`, `metadata`, `allowed-tools`, ...) are read past.
#[derive(serde::Deserialize)]
struct Frontmatter {
    name: String,
    description: String,
}

/// Parses a `SKILL.md`: a YAML frontmatter block between `---` lines, as
/// the Agent Skills spec has it, then the Markdown body. Returns `None` for
/// anything that doesn't match (missing delimiters, frontmatter that is not
/// YAML or lacks `name`/`description`); [`embedded_skills`] `.expect()`s
/// this to succeed for skills this crate ships itself (covered by this
/// module's own test), while [`discover_repository_skills`] treats a `None`
/// here as "skip and warn" for arbitrary repository content.
fn parse_skill_md(source: &str) -> Option<ParsedSkillMd> {
    let source = source.replace("\r\n", "\n");
    let after_open = source.strip_prefix("---\n")?;
    let (frontmatter, body) = match after_open.split_once("\n---\n") {
        Some(split) => split,
        None => (after_open.strip_suffix("\n---")?, ""),
    };
    let fields: Frontmatter = serde_saphyr::from_str(frontmatter).ok()?;
    Some(ParsedSkillMd {
        name: fields.name.trim().to_string(),
        description: fields.description.trim().to_string(),
        body: body.trim_start_matches('\n').to_string(),
    })
}

/// Executes the `skill_read` tool against `registry` (this session's
/// composed [`SkillRegistry`]): returns `id`'s full body (capped at
/// [`SKILL_BODY_CAP_CHARS`]), or an error listing every id this session can
/// see if `id` does not match one. Argument decoding belongs to tools::input.
pub(crate) fn execute_read(registry: &SkillRegistry, id: &str) -> Response {
    match registry.get(id) {
        Some(skill) => {
            let (body, truncated) = cap_to_chars(skill.body(), SKILL_BODY_CAP_CHARS);
            Response::succeeded(SkillRead {
                id: skill.name.clone(),
                description: skill.description.clone(),
                body,
                truncated,
                directory: skill
                    .directory()
                    .map(|directory| directory.display().to_string()),
            })
        }
        None => error_output(format!(
            "unknown skill `{id}` -- available: {}",
            registry.ids().join(", ")
        )),
    }
}

use crate::tools::output::error as error_output;
use crate::tools::output::*;

#[cfg(test)]
mod tests {
    use super::*;

    fn execute_read(registry: &SkillRegistry, raw: &serde_json::Value) -> serde_json::Value {
        crate::tools::test_support::with_input::<crate::tools::input::ReadEntry>(raw, |input| {
            super::execute_read(registry, &input.id)
        })
    }

    fn temp_repo(label: &str) -> tempfile::TempDir {
        let directory = tempfile::Builder::new()
            .prefix(&format!("horizon-agent-skills-{label}-"))
            .tempdir()
            .unwrap();
        std::fs::create_dir(directory.path().join(".git")).unwrap();
        directory
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
    fn parses_frontmatter_the_agent_skills_spec_allows() {
        let source = "---\r\nname: spec-skill\r\ndescription: >\r\n  Folded over\r\n  two lines: with a colon.\r\nlicense: MIT\r\nallowed-tools:\r\n  - Bash\r\n  - Read\r\nmetadata:\r\n  author: someone\r\n---\r\nBody.\r\n";
        let parsed = parse_skill_md(source).expect("spec frontmatter must parse");
        assert_eq!(parsed.name, "spec-skill");
        assert_eq!(parsed.description, "Folded over two lines: with a colon.");
        assert_eq!(parsed.body, "Body.\n");
    }

    #[test]
    fn parses_a_skill_md_with_no_body() {
        let parsed = parse_skill_md("---\nname: bare\ndescription: Nothing more.\n---")
            .expect("a frontmatter-only file must parse");
        assert_eq!(parsed.name, "bare");
        assert_eq!(parsed.body, "");
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
        let registry = SkillRegistry::embedded(&crate::packages::Packages::default());
        let cli_skill = registry
            .get("horizon-cli")
            .expect("horizon-cli must be registered");
        assert!(!cli_skill.description.is_empty());
        assert!(cli_skill.body().contains("horizon"));

        let distill_skill = registry
            .get("horizon-distill")
            .expect("horizon-distill must be registered");
        assert!(!distill_skill.description.is_empty());
        assert!(distill_skill.body().contains("recall_search"));

        let github_pr_skill = registry
            .get("github-pr")
            .expect("github-pr must be registered");
        assert!(!github_pr_skill.description.is_empty());
        assert!(github_pr_skill.body().contains("gh pr create"));
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
        let registry = SkillRegistry::embedded(&crate::packages::Packages::default());
        assert_eq!(registry.prompt_section_for_ids(&[]), None);
    }

    #[test]
    fn prompt_section_for_all_lists_every_skill_and_mentions_skill_read() {
        let registry = SkillRegistry::embedded(&crate::packages::Packages::default());
        let section = registry
            .prompt_section_for_all()
            .expect("must build a section");
        assert!(section.contains("horizon-cli"));
        assert!(section.contains("horizon-cli"));
        assert!(section.contains("github-pr"));
        assert!(section.contains("skill_read"));
    }

    #[test]
    fn prompt_section_for_ids_lists_only_the_given_ids() {
        let registry = SkillRegistry::embedded(&crate::packages::Packages::default());
        let section = registry
            .prompt_section_for_ids(&["horizon-distill"])
            .expect("must build a section");
        assert!(section.contains("horizon-distill"));
        assert!(!section.contains("horizon-cli"));
    }

    #[test]
    fn prompt_section_for_ids_silently_skips_an_unknown_skill_id() {
        let registry = SkillRegistry::embedded(&crate::packages::Packages::default());
        let section = registry
            .prompt_section_for_ids(&["horizon-cli", "does-not-exist"])
            .expect("must still build a section for the known entry");
        assert!(section.contains("horizon-cli"));
        assert!(!section.contains("does-not-exist"));
    }

    #[test]
    fn discover_finds_a_repository_skill_at_the_repo_root() {
        let directory = temp_repo("root-level");
        let root = directory.path().to_path_buf();
        write_skill(&root, "my-skill", "my-skill", "A repo skill.", "Body text.");

        let registry = SkillRegistry::discover(&root, &crate::packages::Packages::default());
        let skill = registry
            .get("my-skill")
            .expect("repo skill must be discovered");
        assert_eq!(skill.description, "A repo skill.");
        assert_eq!(skill.body(), "Body text.");
    }

    #[test]
    fn discover_finds_a_repository_skill_in_a_nested_ancestor() {
        let directory = temp_repo("nested");
        let root = directory.path().to_path_buf();
        let nested = root.join("crates").join("inner");
        std::fs::create_dir_all(&nested).unwrap();
        write_skill(&root, "my-skill", "my-skill", "A repo skill.", "Body text.");

        let registry = SkillRegistry::discover(&nested, &crate::packages::Packages::default());
        assert!(registry.get("my-skill").is_some());
    }

    #[test]
    fn discover_skips_a_skill_dir_with_unparsable_frontmatter() {
        let directory = temp_repo("bad-frontmatter");
        let root = directory.path().to_path_buf();
        let skill_dir = root.join(".horizon").join("skills").join("broken");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(skill_dir.join("SKILL.md"), "not a valid SKILL.md at all").unwrap();

        let registry = SkillRegistry::discover(&root, &crate::packages::Packages::default());
        assert!(registry.get("broken").is_none());
        // The build's embedded skills must still be present -- one bad
        // repository skill must not take down the whole registry.
        assert!(registry.get("horizon-cli").is_some());
    }

    #[test]
    fn discover_does_not_search_ancestors_outside_a_git_repository() {
        let directory = crate::test_support::non_repository_test_dir();
        let root = directory.path().to_path_buf();
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        write_skill(
            &root,
            "parent-only",
            "parent-only",
            "Should not appear.",
            "x",
        );

        let registry = SkillRegistry::discover(&nested, &crate::packages::Packages::default());
        assert!(registry.get("parent-only").is_none());
    }

    #[test]
    fn discover_lets_a_repository_skill_override_an_embedded_one() {
        let directory = temp_repo("override");
        let root = directory.path().to_path_buf();
        write_skill(
            &root,
            "horizon-cli",
            "horizon-cli",
            "Overridden description.",
            "Overridden body.",
        );

        let registry = SkillRegistry::discover(&root, &crate::packages::Packages::default());
        let skill = registry.get("horizon-cli").unwrap();
        assert_eq!(skill.description, "Overridden description.");
        assert_eq!(skill.body(), "Overridden body.");
    }

    #[test]
    fn discover_lets_a_nested_repository_skill_override_a_root_one_with_the_same_id() {
        let directory = temp_repo("nested-override");
        let root = directory.path().to_path_buf();
        let nested = root.join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        write_skill(&root, "dup", "dup", "root version", "root body");
        write_skill(&nested, "dup", "dup", "nested version", "nested body");

        let registry = SkillRegistry::discover(&nested, &crate::packages::Packages::default());
        let skill = registry.get("dup").unwrap();
        assert_eq!(skill.description, "nested version");
    }

    #[test]
    fn embedded_returns_only_embedded_skills_ignoring_repository_skills() {
        let directory = temp_repo("embedded-only");
        let root = directory.path().to_path_buf();
        write_skill(&root, "repo-skill", "repo-skill", "A repo skill.", "Body.");

        let registry = SkillRegistry::embedded(&crate::packages::Packages::default());
        // Embedded skills are present.
        assert!(registry.get("horizon-cli").is_some());
        // Repository skill is NOT present — embedded() does no discovery.
        assert!(registry.get("repo-skill").is_none());
    }

    #[test]
    fn execute_read_returns_the_body_for_a_known_id() {
        let registry = SkillRegistry::embedded(&crate::packages::Packages::default());
        let output = execute_read(&registry, &serde_json::json!({ "id": "horizon-cli" }));
        assert_eq!(output["id"], "horizon-cli");
        assert!(output["body"].as_str().unwrap().contains("horizon"));
        assert_eq!(output["truncated"], false);
    }

    #[test]
    fn execute_read_rereads_a_repository_skill_body_after_it_changes_on_disk() {
        let directory = temp_repo("fresh-reread");
        let root = directory.path().to_path_buf();
        write_skill(&root, "my-skill", "my-skill", "desc", "original body");
        let registry = SkillRegistry::discover(&root, &crate::packages::Packages::default());

        let first = execute_read(&registry, &serde_json::json!({ "id": "my-skill" }));
        assert_eq!(first["body"], "original body");

        write_skill(&root, "my-skill", "my-skill", "desc", "edited body");
        let second = execute_read(&registry, &serde_json::json!({ "id": "my-skill" }));
        assert_eq!(
            second["body"], "edited body",
            "skill_read must re-read a repository skill's body from disk, not a cached copy"
        );
    }

    #[test]
    fn execute_read_lists_available_ids_for_an_unknown_id() {
        let registry = SkillRegistry::embedded(&crate::packages::Packages::default());
        let output = execute_read(&registry, &serde_json::json!({ "id": "no-such-skill" }));
        assert_eq!(output["is_error"], true);
        assert!(output["message"].as_str().unwrap().contains("horizon-cli"));
    }

    #[test]
    fn execute_read_errors_on_a_missing_id_argument() {
        let registry = SkillRegistry::embedded(&crate::packages::Packages::default());
        let output = execute_read(&registry, &serde_json::json!({}));
        assert_eq!(output["is_error"], true);
    }

    #[test]
    fn execute_read_caps_an_oversized_repository_skill_body() {
        let directory = temp_repo("oversized");
        let root = directory.path().to_path_buf();
        let big_body = "x".repeat(SKILL_BODY_CAP_CHARS + 1_000);
        write_skill(&root, "big", "big", "desc", &big_body);
        let registry = SkillRegistry::discover(&root, &crate::packages::Packages::default());

        let output = execute_read(&registry, &serde_json::json!({ "id": "big" }));
        assert_eq!(output["truncated"], true);
        assert!(output["body"].as_str().unwrap().chars().count() <= SKILL_BODY_CAP_CHARS);
    }

    // -- external skill source registration (docs/board-keeper-design.md §1) -

    /// A skill source a package adds is listed beside the embedded skills.
    const TEST_EXTERNAL_SKILL_SOURCE: &str = "---\nname: test-external-skill\ndescription: An externally-registered test skill.\n---\nTest body.\n";

    #[test]
    fn a_package_skill_source_is_listed() {
        let packages = crate::packages::Packages::builder()
            .skill_sources(vec![TEST_EXTERNAL_SKILL_SOURCE])
            .build();
        let registry = SkillRegistry::embedded(&packages);
        let skill = registry
            .get("test-external-skill")
            .expect("external skill source must be embedded after registration");
        assert_eq!(skill.description, "An externally-registered test skill.");
        assert!(skill.body().contains("Test body."));
    }

    #[test]
    fn external_skill_appears_in_prompt_section_for_ids() {
        let packages = crate::packages::Packages::builder()
            .skill_sources(vec![TEST_EXTERNAL_SKILL_SOURCE])
            .build();
        let registry = SkillRegistry::embedded(&packages);
        let section = registry
            .prompt_section_for_ids(&["test-external-skill"])
            .expect("must build a section for the external skill");
        assert!(section.contains("test-external-skill"));
        assert!(section.contains("skill_read"));
    }

    fn write_agents_skill(dir: &Path, id: &str, description: &str) {
        let skill_dir = dir.join(".agents").join("skills").join(id);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {id}\ndescription: {description}\n---\nSee references/guide.md.\n"),
        )
        .unwrap();
    }

    #[test]
    fn discover_finds_skills_under_agents_skills_and_prefers_horizon_skills() {
        let directory = temp_repo("agents-skills");
        let root = directory.path().to_path_buf();
        write_agents_skill(&root, "shared", "From .agents.");
        write_agents_skill(&root, "both", "From .agents.");
        write_skill(&root, "both", "both", "From .horizon.", "Body.");

        let registry = SkillRegistry::discover(&root, &crate::packages::Packages::default());
        assert_eq!(registry.get("shared").unwrap().description, "From .agents.");
        assert_eq!(registry.get("both").unwrap().description, "From .horizon.");
    }

    #[test]
    fn reading_a_repository_skill_names_its_directory() {
        let directory = temp_repo("skill-directory");
        let root = directory.path().to_path_buf();
        write_agents_skill(&root, "guided", "Has a guide.");
        let registry = SkillRegistry::discover(&root, &crate::packages::Packages::default());

        let read = execute_read(&registry, &serde_json::json!({"id": "guided"}));
        let skill_dir = root.join(".agents").join("skills").join("guided");
        assert_eq!(
            read.pointer("/directory")
                .and_then(serde_json::Value::as_str),
            Some(skill_dir.display().to_string().as_str()),
            "{read}"
        );

        let embedded = SkillRegistry::embedded(&crate::packages::Packages::default());
        let id = embedded.ids()[0].to_string();
        let read = execute_read(&embedded, &serde_json::json!({ "id": id }));
        assert!(read.pointer("/directory").is_none(), "{read}");
    }
}
