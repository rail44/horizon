//! Preserve completion precedence independently of process management.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::tools::output::Response;

use super::super::BashCompletion;
use super::super::{
    domain_denied, failed_output, finished, note_undrained, status_output, timeout_output,
};
use super::capture::Captured;
use crate::config::BashToolConfig;
use crate::contract::ToolCallIdentity;
use crate::tools::output::{annotate_denied_domains, annotate_sandboxed};

pub(super) fn complete(
    identity: &ToolCallIdentity,
    captured: Captured,
    denied_domains: Vec<String>,
    git_metadata: &[PathBuf],
    timeout: Duration,
    cwd_handle: &Arc<Mutex<PathBuf>>,
    config: &BashToolConfig,
) -> BashCompletion {
    #[cfg(target_os = "macos")]
    let denial_collection_error = captured.denial_collection_error;
    let Captured {
        status,
        killed,
        drained,
        raw_stdout,
        raw_stderr,
        mut denials,
        ..
    } = captured;
    let git_metadata_denied = divert_git_metadata_denials(&mut denials, git_metadata);
    if killed {
        let mut value = timeout_output(timeout, raw_stdout, config);
        annotate_common(&mut value, &denials, git_metadata_denied);
        if !drained {
            note_undrained(&mut value, Duration::from_secs(config.drain_grace_secs));
        }
        return finish_or_domain_denied(identity, value, denied_domains);
    }
    let Some(status) = status else {
        let mut value = failed_output(
            "failed to wait for sandboxed bash",
            Some(raw_stdout),
            config,
        );
        annotate_common(&mut value, &denials, git_metadata_denied);
        // Wait failures historically carry no drain note or filesystem retry.
        return finish_or_domain_denied(identity, value, denied_domains);
    };

    let mut value = status_output(status, raw_stdout, raw_stderr, cwd_handle, config);
    annotate_common(&mut value, &denials, git_metadata_denied);
    #[cfg(target_os = "macos")]
    if let Some(error) = &denial_collection_error {
        crate::tools::output::annotate_denial_collection_unavailable(&mut value, error);
    }
    if !drained {
        note_undrained(&mut value, Duration::from_secs(config.drain_grace_secs));
    }
    // Filesystem retry takes precedence over domain retry. Both kinds of
    // evidence remain in the output, even if the shell itself exited zero.
    if !denials.filesystem.is_empty() {
        crate::tools::output::annotate_filesystem_denials(&mut value, &denials.filesystem);
        if !denied_domains.is_empty() {
            annotate_denied_domains(&mut value, &denied_domains);
        }
        return BashCompletion::FilesystemDenied {
            denials: denials.filesystem,
            result: value,
        };
    }
    #[cfg(target_os = "macos")]
    if !denials.mach_services.is_empty() {
        crate::tools::output::annotate_denied_mach_services(&mut value, &denials.mach_services);
        if !denied_domains.is_empty() {
            annotate_denied_domains(&mut value, &denied_domains);
        }
        return BashCompletion::MachServiceDenied {
            services: denials.mach_services,
            result: value,
        };
    }
    finish_or_domain_denied(identity, value, denied_domains)
}

fn annotate_common(
    value: &mut Response,
    denials: &horizon_sandbox::ContainmentDenials,
    git_metadata_denied: bool,
) {
    annotate_sandboxed(value, true);
    crate::tools::output::annotate_network_denials(value, &denials.network);
    crate::tools::output::annotate_ungrantable_denials(value, &denials.ungrantable);
    if git_metadata_denied {
        value.mark_failed();
    }
}

const GIT_METADATA_GUIDANCE: &str = "inside the repository's Git metadata, which is not grantable \
     as a filesystem retry; Git metadata is written only through a Git operation approval";

/// Moves filesystem denials under the workspace's Git metadata into the
/// ungrantable set, so they never feed a `FilesystemDenialRetry` grant
/// (board #65, decision 1). Returns whether any denial moved.
fn divert_git_metadata_denials(
    denials: &mut horizon_sandbox::ContainmentDenials,
    git_metadata: &[PathBuf],
) -> bool {
    if git_metadata.is_empty() {
        return false;
    }
    let roots: Vec<PathBuf> = git_metadata
        .iter()
        .flat_map(|path| [Some(path.clone()), path.canonicalize().ok()])
        .flatten()
        .collect();
    let (git, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut denials.filesystem)
        .into_iter()
        .partition(|denial| is_under_any(&denial.attempted_path, &roots));
    denials.filesystem = rest;
    let moved = !git.is_empty();
    for denial in git {
        let entry = horizon_sandbox::UngrantableDenial {
            attempted_path: denial.attempted_path,
            guidance: GIT_METADATA_GUIDANCE.into(),
        };
        if !denials.ungrantable.contains(&entry) {
            denials.ungrantable.push(entry);
        }
    }
    moved
}

/// Upper bound on the direct children of a suggested tree checked for a
/// `.git` entry, so a tree with a huge top level stays a bounded scan.
const NESTED_REPOSITORY_SCAN_LIMIT: usize = 1024;

/// Adds the Git metadata a suggested writable tree contains to that grant's
/// `excluded_subpaths`, so a filesystem retry never opens Git metadata
/// (board #71); Git metadata is written only through a `GitOperation`
/// approval. Covered paths are the session repository's metadata as
/// `git_metadata_paths` resolves it from `workspace_root` (a plain
/// checkout's `.git`, or a linked worktree's `.git` pointer, its gitdir, and
/// the common gitdir), plus a `.git` entry at the tree itself or at one of
/// its direct children (at most `NESTED_REPOSITORY_SCAN_LIMIT` of them).
/// Repositories nested deeper than that are not scanned for.
pub fn exclude_git_metadata(
    grants: &mut [horizon_sandbox::FilesystemGrant],
    workspace_root: Option<&Path>,
) {
    let session_metadata: Vec<PathBuf> = workspace_root
        .map(super::git_metadata_paths)
        .unwrap_or_default()
        .into_iter()
        .flat_map(|path| [path.canonicalize().ok(), Some(path)])
        .flatten()
        .collect();
    for grant in grants.iter_mut().filter(|grant| {
        grant.access == horizon_sandbox::FilesystemGrantAccess::ReadWrite
            && grant.scope == horizon_sandbox::FilesystemGrantScope::DirectoryTree
    }) {
        let Ok(tree) = grant.path.canonicalize() else {
            continue;
        };
        let contained: Vec<PathBuf> = session_metadata
            .iter()
            .cloned()
            .chain(nested_dot_git_entries(&tree))
            .filter_map(|metadata| Some(metadata.strip_prefix(&tree).ok()?.to_path_buf()))
            .filter(|relative| !relative.as_os_str().is_empty())
            .collect();
        // A linked worktree's gitdir sits inside the common gitdir; the
        // outermost path alone is the exclusion.
        for relative in &contained {
            let covered = contained
                .iter()
                .any(|other| other != relative && relative.starts_with(other));
            if !covered && !grant.excluded_subpaths.contains(relative) {
                grant.excluded_subpaths.push(relative.clone());
            }
        }
    }
}

/// `tree/.git` and `tree/<child>/.git`, for whichever of them exist.
fn nested_dot_git_entries(tree: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let own = tree.join(".git");
    if std::fs::symlink_metadata(&own).is_ok() {
        found.push(own);
    }
    let Ok(children) = std::fs::read_dir(tree) else {
        return found;
    };
    for child in children
        .flatten()
        .take(NESTED_REPOSITORY_SCAN_LIMIT)
        .filter(|child| child.file_type().is_ok_and(|kind| kind.is_dir()))
    {
        let dot_git = child.path().join(".git");
        if std::fs::symlink_metadata(&dot_git).is_ok() {
            found.push(dot_git);
        }
    }
    found
}

/// Whether `path` (or, when its leaf does not exist yet, its nearest
/// existing ancestor once canonicalized) lies under one of `roots`.
fn is_under_any(path: &Path, roots: &[PathBuf]) -> bool {
    if roots.iter().any(|root| path.starts_with(root)) {
        return true;
    }
    let mut existing = path;
    let mut rest = Vec::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            let resolved = rest
                .iter()
                .rev()
                .fold(canonical, |acc: PathBuf, part| acc.join(part));
            return roots.iter().any(|root| resolved.starts_with(root));
        }
        let (Some(parent), Some(name)) = (existing.parent(), existing.file_name()) else {
            return false;
        };
        rest.push(name);
        existing = parent;
    }
}

fn finish_or_domain_denied(
    identity: &ToolCallIdentity,
    mut value: Response,
    domains: Vec<String>,
) -> BashCompletion {
    if domains.is_empty() {
        finished(identity, value)
    } else {
        annotate_denied_domains(&mut value, &domains);
        domain_denied(identity, domains, value)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::tools::bash::BashCompletion;
    use horizon_sandbox::{
        ContainmentDenials, FilesystemDenial, FilesystemGrant, FilesystemGrantAccess,
        FilesystemGrantScope, NetworkDenial, UngrantableDenial,
    };
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    fn captured(status: Option<i32>, killed: bool) -> Captured {
        Captured {
            status: status.map(ExitStatus::from_raw),
            killed,
            drained: false,
            raw_stdout: Vec::new(),
            raw_stderr: b"/workspace/next".to_vec(),
            denials: ContainmentDenials {
                filesystem: vec![FilesystemDenial {
                    attempted_path: "/outside/file".into(),
                    grant: FilesystemGrant {
                        path: "/outside/file".into(),
                        access: FilesystemGrantAccess::ReadWrite,
                        scope: FilesystemGrantScope::File,
                        excluded_subpaths: Vec::new(),
                    },
                }],
                network: vec![NetworkDenial {
                    target: "192.0.2.1:443".into(),
                    operation: "connect".into(),
                    reason: "direct egress denied".into(),
                }],
                ungrantable: vec![UngrantableDenial {
                    attempted_path: "/".into(),
                    guidance: "choose a narrower path".into(),
                }],
                ..Default::default()
            },
            #[cfg(target_os = "macos")]
            denial_collection_error: None,
        }
    }

    fn finish(captured: Captured, domains: Vec<String>) -> (BashCompletion, PathBuf) {
        finish_with(captured, domains, &[])
    }

    fn finish_with(
        captured: Captured,
        domains: Vec<String>,
        git_metadata: &[PathBuf],
    ) -> (BashCompletion, PathBuf) {
        let cwd = Arc::new(Mutex::new(PathBuf::from("/workspace")));
        let identity = crate::test_support::tool_identity(&crate::contract::ToolCallId(
            "result-precedence".into(),
        ));
        let completion = complete(
            &identity,
            captured,
            domains,
            git_metadata,
            Duration::from_secs(1),
            &cwd,
            &crate::config::AgentToolsConfig::default().bash,
        );
        let final_cwd = cwd.lock().unwrap().clone();
        (
            completion.map_result(|output| identity.finish(output)),
            final_cwd,
        )
    }

    #[test]
    fn filesystem_retry_wins_over_domain_retry_even_after_successful_exit() {
        let capture = captured(Some(0), false);
        let expected_denials = capture.denials.filesystem.clone();
        let (completion, cwd) = finish(capture, vec!["example.test".into()]);
        let BashCompletion::FilesystemDenied {
            result, denials, ..
        } = completion
        else {
            panic!("filesystem denial must take precedence");
        };
        assert_eq!(denials, expected_denials);
        assert!(result.is_error());
        assert_eq!(result.occurrence_id.0, "result-precedence");
        assert_eq!(result.output["exit_code"], 0);
        assert_eq!(result.output["sandboxed"], true);
        assert_eq!(result.output["denied_domains"][0], "example.test");
        assert_eq!(result.output["denied_filesystem_paths"][0], "/outside/file");
        assert_eq!(
            result.output["denied_network_routes"][0]["target"],
            "192.0.2.1:443"
        );
        assert_eq!(
            result.output["ungrantable_filesystem_paths"][0]["path"],
            "/"
        );
        assert!(result.output["note"]
            .as_str()
            .unwrap()
            .contains("output capture stopped"));
        assert_eq!(cwd, PathBuf::from("/workspace/next"));
    }

    #[test]
    fn timeout_and_wait_failure_keep_domain_evidence_without_filesystem_retry() {
        for killed in [false, true] {
            let (completion, cwd) = finish(captured(None, killed), vec!["example.test".into()]);
            let BashCompletion::DomainDenied {
                result, domains, ..
            } = completion
            else {
                panic!("timeout and wait failure must not request filesystem retry");
            };
            assert_eq!(domains, ["example.test"]);
            assert!(result.is_error());
            assert_eq!(result.output["sandboxed"], true);
            assert_eq!(
                result.output["denied_network_routes"][0]["operation"],
                "connect"
            );
            assert_eq!(
                result.output["ungrantable_filesystem_paths"][0]["path"],
                "/"
            );
            assert!(result.output.get("denied_filesystem_paths").is_none());
            assert_eq!(result.output.get("note").is_some(), killed);
            assert!(result.output["message"]
                .as_str()
                .unwrap()
                .contains(if killed {
                    "timed out"
                } else {
                    "failed to wait"
                }));
            assert_eq!(cwd, PathBuf::from("/workspace"));
        }
    }

    #[test]
    fn domain_denial_overrides_success_while_plain_exit_and_signal_keep_their_meaning() {
        for (raw_status, domains) in [
            (0, vec!["example.test".into()]),
            (7 << 8, vec![]),
            (9, vec![]),
        ] {
            let mut capture = captured(Some(raw_status), false);
            capture.denials = ContainmentDenials::default();
            capture.drained = true;
            let (completion, cwd) = finish(capture, domains);
            let result = match completion {
                BashCompletion::DomainDenied { result, .. } if raw_status == 0 => result,
                BashCompletion::Finished(result) if raw_status != 0 => result,
                _ => panic!("unexpected completion"),
            };
            assert_eq!(result.is_error(), raw_status != 7 << 8);
            assert_eq!(result.output["sandboxed"], true);
            assert!(result.output.get("note").is_none());
            if raw_status == 9 {
                assert_eq!(cwd, PathBuf::from("/workspace"));
                assert!(result.output["message"]
                    .as_str()
                    .unwrap()
                    .contains("signal 9"));
            } else {
                assert_eq!(result.output["exit_code"], raw_status >> 8);
                assert_eq!(cwd, PathBuf::from("/workspace/next"));
            }
        }
    }

    fn write_denial(path: &Path) -> FilesystemDenial {
        FilesystemDenial {
            attempted_path: path.to_path_buf(),
            grant: FilesystemGrant {
                path: path.to_path_buf(),
                access: FilesystemGrantAccess::ReadWrite,
                scope: FilesystemGrantScope::File,
                excluded_subpaths: Vec::new(),
            },
        }
    }

    fn with_filesystem_denials(paths: &[&Path]) -> Captured {
        let mut capture = captured(Some(1 << 8), false);
        capture.denials = ContainmentDenials {
            filesystem: paths.iter().map(|path| write_denial(path)).collect(),
            ..Default::default()
        };
        capture
    }

    fn plain_checkout(root: &Path) -> PathBuf {
        let workspace = root.join("repo");
        let git = workspace.join(".git");
        std::fs::create_dir_all(git.join("objects")).unwrap();
        std::fs::create_dir_all(git.join("refs")).unwrap();
        std::fs::create_dir_all(git.join("hooks")).unwrap();
        std::fs::write(git.join("HEAD"), "ref: refs/heads/main\n").unwrap();
        workspace.canonicalize().unwrap()
    }

    fn assert_git_metadata_denial(completion: BashCompletion, path: &Path) {
        let BashCompletion::Finished(result) = completion else {
            panic!("a Git metadata denial must not request a filesystem retry: {completion:?}");
        };
        assert!(result.is_error());
        assert!(result.output.get("denied_filesystem_paths").is_none());
        let ungrantable = &result.output["ungrantable_filesystem_paths"][0];
        assert_eq!(ungrantable["path"], path.display().to_string());
        assert_eq!(ungrantable["guidance"], GIT_METADATA_GUIDANCE);
        assert!(GIT_METADATA_GUIDANCE.contains("Git metadata"));
    }

    #[test]
    fn plain_checkout_git_metadata_denial_requests_no_retry() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = plain_checkout(dir.path());
        let attempted = workspace.join(".git/hooks/x");
        let (completion, _) = finish_with(
            with_filesystem_denials(&[&attempted]),
            Vec::new(),
            &super::super::git_metadata_paths(&workspace),
        );
        assert_git_metadata_denial(completion, &attempted);
    }

    #[test]
    fn linked_worktree_gitdir_denial_requests_no_retry() {
        let dir = tempfile::tempdir().unwrap();
        let main = plain_checkout(dir.path());
        let worktree_git_dir = main.join(".git/worktrees/agent");
        std::fs::create_dir_all(&worktree_git_dir).unwrap();
        let workspace = dir.path().join("worktree");
        std::fs::create_dir_all(&workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        std::fs::write(
            workspace.join(".git"),
            format!("gitdir: {}\n", worktree_git_dir.display()),
        )
        .unwrap();
        std::fs::write(
            worktree_git_dir.join("gitdir"),
            workspace.join(".git").display().to_string(),
        )
        .unwrap();
        std::fs::write(worktree_git_dir.join("commondir"), "../..\n").unwrap();
        let git_metadata = super::super::git_metadata_paths(&workspace);
        for attempted in [
            worktree_git_dir.join("index.lock"),
            main.join(".git/refs/x"),
        ] {
            let (completion, _) = finish_with(
                with_filesystem_denials(&[&attempted]),
                Vec::new(),
                &git_metadata,
            );
            assert_git_metadata_denial(completion, &attempted);
        }
    }

    #[test]
    fn denials_outside_git_metadata_still_request_retry() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = plain_checkout(dir.path());
        let git_metadata = super::super::git_metadata_paths(&workspace);
        let cargo = dir.path().join("home/.cargo/registry/x");
        let in_tree = workspace.join("src/.gitignore");
        for attempted in [cargo, in_tree] {
            let capture = with_filesystem_denials(&[&attempted]);
            let expected = capture.denials.filesystem.clone();
            let (completion, _) = finish_with(capture, Vec::new(), &git_metadata);
            let BashCompletion::FilesystemDenied { denials, result } = completion else {
                panic!("an ordinary denial must still request a filesystem retry");
            };
            assert_eq!(denials, expected);
            assert!(result.output.get("ungrantable_filesystem_paths").is_none());
        }
    }

    #[test]
    fn mixed_denials_retry_only_the_ordinary_one() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = plain_checkout(dir.path());
        let git_attempt = workspace.join(".git/config");
        let ordinary = dir.path().join("outside/cache/lock");
        std::fs::create_dir_all(ordinary.parent().unwrap()).unwrap();
        let (completion, _) = finish_with(
            with_filesystem_denials(&[&git_attempt, &ordinary]),
            Vec::new(),
            &super::super::git_metadata_paths(&workspace),
        );
        let BashCompletion::FilesystemDenied { denials, result } = completion else {
            panic!("the ordinary denial must still request a filesystem retry");
        };
        assert_eq!(denials, vec![write_denial(&ordinary)]);
        assert_eq!(
            result.output["ungrantable_filesystem_paths"][0]["path"],
            git_attempt.display().to_string()
        );
        let grants = horizon_sandbox::suggest_grants(&denials, Some(&workspace), None);
        assert_eq!(grants.len(), 1);
        assert!(!grants[0].path.starts_with(workspace.join(".git")));
        assert!(ordinary.starts_with(&grants[0].path));
    }

    /// The grants a filesystem retry would offer for `attempted`.
    fn suggested(attempted: &[&Path], workspace: Option<&Path>) -> Vec<FilesystemGrant> {
        let denials: Vec<_> = attempted.iter().map(|path| write_denial(path)).collect();
        let mut grants = horizon_sandbox::suggest_grants(&denials, workspace, None);
        exclude_git_metadata(&mut grants, workspace);
        grants
    }

    fn tree(path: &Path, excluded: &[&str]) -> FilesystemGrant {
        FilesystemGrant {
            path: path.to_path_buf(),
            access: FilesystemGrantAccess::ReadWrite,
            scope: FilesystemGrantScope::DirectoryTree,
            excluded_subpaths: excluded.iter().map(PathBuf::from).collect(),
        }
    }

    /// Registers `workspace` as a linked worktree of the plain checkout at
    /// `main`, returning the worktree's canonical path.
    fn linked_worktree(main: &Path, workspace: &Path) -> PathBuf {
        let worktree_git_dir = main.join(".git/worktrees/agent");
        std::fs::create_dir_all(&worktree_git_dir).unwrap();
        std::fs::create_dir_all(workspace).unwrap();
        let workspace = workspace.canonicalize().unwrap();
        std::fs::write(
            workspace.join(".git"),
            format!("gitdir: {}\n", worktree_git_dir.display()),
        )
        .unwrap();
        std::fs::write(
            worktree_git_dir.join("gitdir"),
            workspace.join(".git").display().to_string(),
        )
        .unwrap();
        std::fs::write(worktree_git_dir.join("commondir"), "../..\n").unwrap();
        workspace
    }

    #[test]
    fn retry_tree_around_the_main_checkout_excludes_its_gitdir() {
        let dir = tempfile::tempdir().unwrap();
        let main = plain_checkout(dir.path());
        let workspace = linked_worktree(&main, &dir.path().join("worktree"));
        let attempted = main.join("Cargo.lock");
        assert_eq!(
            suggested(&[&attempted], Some(&workspace)),
            vec![tree(&main, &[".git"])]
        );
    }

    #[test]
    fn session_gitdirs_are_excluded_below_the_scanned_depth() {
        let dir = tempfile::tempdir().unwrap();
        let nest = dir.path().join("nest");
        std::fs::create_dir_all(nest.join("deep")).unwrap();
        std::fs::create_dir_all(nest.join("other")).unwrap();
        let main = plain_checkout(&nest.join("deep"));
        let workspace = linked_worktree(&main, &dir.path().join("worktree"));
        let grants = suggested(
            &[&main.join("Cargo.lock"), &nest.join("other/lock")],
            Some(&workspace),
        );
        assert_eq!(
            grants,
            vec![tree(&nest.canonicalize().unwrap(), &["deep/repo/.git"])]
        );
    }

    #[test]
    fn retry_tree_excludes_every_direct_child_repository() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let main = plain_checkout(&root);
        let workspace = linked_worktree(&main, &root.join("worktree"));
        let other = root.join("other");
        std::fs::create_dir_all(other.join(".git")).unwrap();
        let grants = suggested(
            &[&main.join("Cargo.lock"), &other.join("lock")],
            Some(&workspace),
        );
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].path, root);
        let mut excluded = grants[0].excluded_subpaths.clone();
        excluded.sort();
        assert_eq!(
            excluded,
            ["other/.git", "repo/.git", "worktree/.git"].map(PathBuf::from)
        );
    }

    #[test]
    fn plain_checkout_tree_excludes_git_only_when_it_contains_it() {
        let dir = tempfile::tempdir().unwrap();
        let checkout = plain_checkout(dir.path());
        std::fs::create_dir_all(checkout.join("src")).unwrap();
        assert_eq!(
            suggested(&[&checkout.join("Cargo.lock")], None),
            vec![tree(&checkout, &[".git"])]
        );
        assert_eq!(
            suggested(&[&checkout.join("src/lib.rs")], None),
            vec![tree(&checkout.join("src"), &[])]
        );
    }

    #[test]
    fn tree_without_a_repository_gets_no_exclusion() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = plain_checkout(dir.path());
        let cargo = dir.path().join("home/.cargo");
        std::fs::create_dir_all(cargo.join("registry")).unwrap();
        std::fs::create_dir_all(cargo.join("git")).unwrap();
        let grants = suggested(
            &[
                &cargo.join(".package-cache-mutate"),
                &cargo.join("registry/lock"),
            ],
            Some(&workspace),
        );
        assert_eq!(grants, vec![tree(&cargo.canonicalize().unwrap(), &[])]);
    }
}
