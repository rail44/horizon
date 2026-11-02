//! `code-db index`: jscpd's two reports over the source files, one
//! rust-analyzer SCIP index per Cargo workspace and one scip-go index per Go
//! module, each tool under a memory cap (duckdb-ext/sql/README.md, Running and
//! Limits).

use std::fs::File;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::time::Instant;

use crate::limits::mem_available;

/// How a tool's memory is capped.
enum Cap {
    None,
    /// `systemd-run --user --scope -p MemoryMax`: the resident memory of all
    /// the tool's processes together.
    Systemd(u64),
    /// `prlimit --as`: the address space of each process.
    Prlimit(u64),
}

/// SIZE (a number with an optional K, M, G or T suffix, base 1024, as
/// systemd's MemoryMax takes it) in bytes.
pub fn to_bytes(size: &str) -> Option<u64> {
    let (number, factor) = match size.chars().last()?.to_ascii_uppercase() {
        'K' => (&size[..size.len() - 1], 1024f64),
        'M' => (&size[..size.len() - 1], 1024f64.powi(2)),
        'G' => (&size[..size.len() - 1], 1024f64.powi(3)),
        'T' => (&size[..size.len() - 1], 1024f64.powi(4)),
        _ => (size, 1.0),
    };
    if number.is_empty()
        || !number.chars().all(|c| c.is_ascii_digit() || c == '.')
        || number.starts_with('.')
        || number.matches('.').count() > 1
        || number.ends_with('.')
    {
        return None;
    }
    number
        .parse::<f64>()
        .ok()
        .map(|n| (n * factor).round() as u64)
}

fn human(bytes: u64) -> String {
    format!("{:.1} GB", bytes as f64 / 1e9)
}

/// Whether PROGRAM is an executable on PATH.
pub fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| {
        std::env::split_paths(&path).any(|dir| {
            let candidate = dir.join(program);
            candidate.is_file()
                && std::fs::metadata(&candidate)
                    .map(|m| {
                        use std::os::unix::fs::PermissionsExt;
                        m.permissions().mode() & 0o111 != 0
                    })
                    .unwrap_or(false)
        })
    })
}

/// The cap is tool_memory_max, else 80 % of MemAvailable; the mechanism is
/// systemd-run where a user manager answers, else prlimit, else none.
fn choose_cap(tool_memory_max: Option<&str>) -> Result<Cap, String> {
    let (bytes, why) = match tool_memory_max {
        Some(size) => {
            let bytes = to_bytes(size)
                .ok_or_else(|| format!("tool_memory_max must be a size such as 8G: {size}"))?;
            (Some(bytes), format!("tool_memory_max={size}"))
        }
        None => match std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|m| mem_available(&m))
        {
            Some(available) => (
                Some((available * 0.8).round() as u64),
                format!("80% of {} available", human(available as u64)),
            ),
            None => (None, String::new()),
        },
    };
    let Some(bytes) = bytes else {
        eprintln!(
            "code-db: warning: /proc/meminfo unreadable and tool_memory_max unset; the tools run without a memory cap"
        );
        return Ok(Cap::None);
    };
    let systemd = on_path("systemd-run")
        && Command::new("systemd-run")
            .args(["--user", "--scope", "--quiet", "true"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
    if systemd {
        eprintln!(
            "tool memory cap: {} ({why}), systemd-run --user --scope -p MemoryMax (resident memory of the tool's processes)",
            human(bytes)
        );
        Ok(Cap::Systemd(bytes))
    } else if on_path("prlimit") {
        eprintln!(
            "tool memory cap: {} ({why}), prlimit --as (address space of each process, not resident memory)",
            human(bytes)
        );
        Ok(Cap::Prlimit(bytes))
    } else {
        eprintln!(
            "code-db: warning: neither systemd-run --user nor prlimit is available; the tools run without a memory cap"
        );
        Ok(Cap::None)
    }
}

fn capped(cap: &Cap, program: &Path, args: &[String]) -> Command {
    match cap {
        Cap::Systemd(bytes) => {
            let mut c = Command::new("systemd-run");
            c.args(["--user", "--scope", "--quiet", "-p"])
                .arg(format!("MemoryMax={bytes}"))
                .args(["-p", "MemorySwapMax=0", "--"])
                .arg(program)
                .args(args);
            c
        }
        Cap::Prlimit(bytes) => {
            let mut c = Command::new("prlimit");
            c.arg(format!("--as={bytes}"))
                .arg("--")
                .arg(program)
                .args(args);
            c
        }
        Cap::None => {
            let mut c = Command::new(program);
            c.args(args);
            c
        }
    }
}

/// The one-line error for a tool that ended with STATUS.
fn failed(tool: &str, status: ExitStatus, cap: &Cap, log: &Path) -> String {
    use std::os::unix::process::ExitStatusExt;
    let how = match (status.code(), status.signal()) {
        (Some(code), _) if code > 128 => format!("was killed by signal {}", code - 128),
        (Some(code), _) => format!("exited with status {code}"),
        (None, Some(signal)) => format!("was killed by signal {signal}"),
        (None, None) => "failed".to_owned(),
    };
    match cap {
        Cap::None => format!("{tool} {how}; see {}", log.display()),
        Cap::Systemd(bytes) | Cap::Prlimit(bytes) => format!(
            "{tool} {how} under the {} memory cap ({}; raise it with tool_memory_max=SIZE if the log shows an allocation failure); see {}",
            human(*bytes),
            if matches!(cap, Cap::Systemd(_)) {
                "systemd"
            } else {
                "prlimit"
            },
            log.display()
        ),
    }
}

fn run_logged(mut command: Command, stdout: Stdio, log: File) -> Result<ExitStatus, String> {
    command
        .stdout(stdout)
        .stderr(log)
        .status()
        .map_err(|e| e.to_string())
}

/// What `index` works on: the source files (`files`, as the views list them:
/// the settings' scope applied), the Cargo workspaces and the scip-go runs, as
/// repository-relative paths; a workspace is a directory whose tracked
/// Cargo.toml has a `[workspace]` table and that holds at least one source
/// file ("" for the repository root).
pub struct Inputs {
    pub files: Vec<String>,
    pub workspaces: Vec<String>,
    pub go_indexes: Vec<GoIndex>,
}

/// One scip-go run: in DIR over the modules MODULES (repository-relative), with
/// GOWORK set to WORK (the tracked go.work in DIR whose `use` lists them) or to
/// `off` for a module no tracked go.work uses.
#[derive(Debug, PartialEq, Eq)]
pub struct GoIndex {
    pub dir: String,
    pub modules: Vec<String>,
    pub work: Option<String>,
}

impl GoIndex {
    /// Its index file under `target/code-db/scip/`: a module's
    /// (go_index_name), or for a go.work `gowork.scip` at the repository root
    /// and `gowork-<path with / as +>.scip` elsewhere.
    pub fn name(&self) -> String {
        match (&self.work, self.dir.is_empty()) {
            (None, _) => go_index_name(&self.dir),
            (Some(_), true) => "gowork.scip".to_owned(),
            (Some(_), false) => format!("gowork-{}.scip", self.dir.replace('/', "+")),
        }
    }

    /// The package patterns: each module's packages, relative to DIR.
    fn patterns(&self) -> Vec<String> {
        self.modules
            .iter()
            .map(|m| {
                let rel = if self.dir.is_empty() {
                    m.as_str()
                } else if m == &self.dir {
                    ""
                } else {
                    &m[self.dir.len() + 1..]
                };
                if rel.is_empty() {
                    "./...".to_owned()
                } else {
                    format!("./{rel}/...")
                }
            })
            .collect()
    }
}

/// The directories a go.work's `use` directives name, as written (`use ./a`,
/// `use ( ./a "./b" )`, comments after `//` ignored).
pub fn work_uses(text: &str) -> Vec<String> {
    let mut uses = Vec::new();
    let mut in_block = false;
    for line in text.lines() {
        let line = line.split("//").next().unwrap_or_default().trim();
        let path = if in_block {
            if line.starts_with(')') {
                in_block = false;
                continue;
            }
            line
        } else if let Some(rest) = line
            .strip_prefix("use")
            .filter(|r| r.starts_with([' ', '\t', '(']))
        {
            let rest = rest.trim();
            match rest.strip_prefix('(') {
                Some(inner) => match inner.strip_suffix(')') {
                    Some(one) => one.trim(),
                    None => {
                        in_block = true;
                        inner.trim()
                    }
                },
                None => rest,
            }
        } else {
            continue;
        };
        let path = path.trim_matches(|c| c == '"' || c == '`');
        if !path.is_empty() {
            uses.push(path.to_owned());
        }
    }
    uses
}

/// DIR joined with the relative path PATH, `.` and `..` resolved; None when it
/// leaves the repository or PATH is absolute.
fn join_relative(dir: &str, path: &str) -> Option<String> {
    if path.starts_with('/') {
        return None;
    }
    let mut parts: Vec<&str> = dir.split('/').filter(|p| !p.is_empty()).collect();
    for part in path.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop()?;
            }
            part => parts.push(part),
        }
    }
    Some(parts.join("/"))
}

/// The scip-go runs for MODULES (go_modules) given the tracked go.work files
/// WORKS as (path, text): a module a go.work uses is indexed with the others it
/// uses, from the go.work's directory, so that their references to each other
/// name the same symbols (indexed one by one, scip-go names another workspace
/// module's symbols with version `.`); when several go.work files use it, the
/// deepest one does. Every other module is indexed on its own.
pub fn go_indexes(modules: &[String], works: &[(String, String)]) -> Vec<GoIndex> {
    let mut indexes: Vec<GoIndex> = Vec::new();
    for module in modules {
        let work = works
            .iter()
            .filter_map(|(path, text)| {
                let dir = path
                    .strip_suffix("go.work")
                    .unwrap_or_default()
                    .trim_end_matches('/');
                work_uses(text)
                    .iter()
                    .any(|u| join_relative(dir, u).as_deref() == Some(module.as_str()))
                    .then(|| (dir.to_owned(), path.clone()))
            })
            .max_by_key(|(dir, _)| dir.len());
        match work {
            Some((dir, path)) => {
                match indexes.iter_mut().find(|i| i.work.as_ref() == Some(&path)) {
                    Some(index) => index.modules.push(module.clone()),
                    None => indexes.push(GoIndex {
                        dir,
                        modules: vec![module.clone()],
                        work: Some(path),
                    }),
                }
            }
            None => indexes.push(GoIndex {
                dir: module.clone(),
                modules: vec![module.clone()],
                work: None,
            }),
        }
    }
    indexes
}

/// Whether a Cargo.toml's text declares a workspace (`[workspace]` or one of
/// its subtables such as `[workspace.package]`).
pub fn declares_workspace(manifest: &str) -> bool {
    manifest.lines().any(|line| {
        let line = line.trim();
        line == "[workspace]" || line.starts_with("[workspace.")
    })
}

/// The index file of a workspace, under `target/code-db/scip/`: `root.scip`
/// for the repository root, else `ws-<path with / as +>.scip`.
pub fn index_name(workspace: &str) -> String {
    if workspace.is_empty() {
        "root.scip".to_owned()
    } else {
        format!("ws-{}.scip", workspace.replace('/', "+"))
    }
}

fn under(path: &str, dir: &str) -> bool {
    dir.is_empty()
        || path
            .strip_prefix(dir)
            .is_some_and(|rest| rest.starts_with('/'))
}

/// The index file of a Go module, under `target/code-db/scip/`: `go.scip` for
/// the repository root, else `go-<path with / as +>.scip`.
pub fn go_index_name(module: &str) -> String {
    if module.is_empty() {
        "go.scip".to_owned()
    } else {
        format!("go-{}.scip", module.replace('/', "+"))
    }
}

/// The Go modules among the tracked go.mod files that hold a `.go` source file.
pub fn go_modules(go_mods: &[String], files: &[String]) -> Vec<String> {
    let mut found: Vec<String> = go_mods
        .iter()
        .map(|m| {
            m.strip_suffix("go.mod")
                .unwrap_or_default()
                .trim_end_matches('/')
                .to_owned()
        })
        .filter(|dir| files.iter().any(|f| f.ends_with(".go") && under(f, dir)))
        .collect();
    found.sort();
    found
}

/// The workspaces among the tracked manifests that hold a source file.
pub fn workspaces(repo: &Path, manifests: &[String], files: &[String]) -> Vec<String> {
    let mut found: Vec<String> = manifests
        .iter()
        .filter(|m| {
            std::fs::read_to_string(repo.join(m)).is_ok_and(|text| declares_workspace(&text))
        })
        .map(|m| {
            m.strip_suffix("Cargo.toml")
                .unwrap_or_default()
                .trim_end_matches('/')
                .to_owned()
        })
        .filter(|dir| files.iter().any(|f| under(f, dir)))
        .collect();
    found.sort();
    found
}

/// A program given by the environment variable VARIABLE (a path, or a name on
/// PATH), else DEFAULT on PATH.
fn tool(variable: &str, default: &str) -> Result<PathBuf, String> {
    let name = std::env::var(variable).unwrap_or_else(|_| default.to_owned());
    let path = PathBuf::from(&name);
    if name.contains('/') && path.is_file() || on_path(&name) {
        Ok(path)
    } else {
        Err(format!("{default} not found (set {variable})"))
    }
}

/// An indexer: its name in messages, its program and the arguments before
/// `--output FILE`, which both indexers take.
struct Indexer<'a> {
    label: &'a str,
    program: PathBuf,
    args: &'a [&'a str],
}

/// Run INDEXER in the repository directory DIR under the cap, with the
/// arguments EXTRA after its own and the environment ENV, writing its index to
/// `OUT/scip/NAME` through a temporary file and its output to
/// `OUT/logs/NAME.log`.
#[allow(clippy::too_many_arguments)]
fn run_indexer(
    cap: &Cap,
    out: &Path,
    indexer: &Indexer,
    repo: &Path,
    dir: &str,
    name: &str,
    extra: &[String],
    env: &[(&str, String)],
) -> Result<(), String> {
    let start = Instant::now();
    let shown = if dir.is_empty() { "." } else { dir };
    let log = out.join("logs").join(format!("{name}.log"));
    let tmp = out.join("scip").join(format!("{name}.tmp"));
    let mut args: Vec<String> = indexer.args.iter().map(|a| (*a).to_owned()).collect();
    args.extend(extra.iter().cloned());
    args.extend(["--output".to_owned(), tmp.display().to_string()]);
    let log_file = File::create(&log).map_err(|e| e.to_string())?;
    let stdout = Stdio::from(log_file.try_clone().map_err(|e| e.to_string())?);
    let mut command = capped(cap, &indexer.program, &args);
    command.current_dir(repo.join(dir));
    command.envs(env.iter().map(|(k, v)| (*k, v)));
    let status = run_logged(command, stdout, log_file)?;
    if !status.success() {
        return Err(failed(
            &format!("{} in {shown}", indexer.label),
            status,
            cap,
            &log,
        ));
    }
    let index = out.join("scip").join(name);
    std::fs::rename(&tmp, &index).map_err(|e| e.to_string())?;
    eprintln!(
        "{} ({shown}): {:.1} s -> {} (log {})",
        indexer.label,
        start.elapsed().as_secs_f64(),
        index.display(),
        log.display()
    );
    Ok(())
}

pub fn index(
    root: &Path,
    repo: &Path,
    inputs: &Inputs,
    tool_memory_max: Option<&str>,
) -> Result<(), String> {
    let out = repo.join("target/code-db");
    let logs = out.join("logs");
    std::fs::create_dir_all(&logs).map_err(|e| e.to_string())?;
    let cap = choose_cap(tool_memory_max)?;

    if inputs.files.is_empty() {
        return Err(format!("no source file in {}", repo.display()));
    }
    if !root.join("target/refactor-audit/tools/jscpd").exists()
        && std::env::var_os("JSCPD").is_none()
    {
        return Err(format!(
            "jscpd not installed: run 'uv run --locked --script scripts/refactor-audit/run.py setup' in {}",
            root.display()
        ));
    }
    let start = Instant::now();
    let log = logs.join("jscpd.log");
    let mut args = vec![
        repo.display().to_string(),
        out.join("jscpd").display().to_string(),
    ];
    args.extend(inputs.files.iter().cloned());
    let jscpd = root.join("duckdb-ext/sql/jscpd.sh");
    let log_file = File::create(&log).map_err(|e| e.to_string())?;
    let status = run_logged(capped(&cap, &jscpd, &args), Stdio::null(), log_file)?;
    if !status.success() {
        return Err(failed("jscpd", status, &cap, &log));
    }
    eprintln!(
        "jscpd ({} files): {:.1} s -> {} (log {})",
        inputs.files.len(),
        start.elapsed().as_secs_f64(),
        out.join("jscpd").display(),
        log.display()
    );

    let scip = out.join("scip");
    std::fs::create_dir_all(&scip).map_err(|e| e.to_string())?;
    let mut names: Vec<String> = Vec::new();
    if !inputs.workspaces.is_empty() {
        let indexer = Indexer {
            label: "rust-analyzer scip",
            program: tool("RUST_ANALYZER", "rust-analyzer")?,
            args: &["scip", "."],
        };
        for workspace in &inputs.workspaces {
            let name = index_name(workspace);
            run_indexer(&cap, &out, &indexer, repo, workspace, &name, &[], &[])?;
            names.push(name);
        }
    }
    if !inputs.go_indexes.is_empty() {
        let indexer = Indexer {
            label: "scip-go index",
            program: tool("SCIP_GO", "scip-go")?,
            args: &["index", "--quiet"],
        };
        for go in &inputs.go_indexes {
            let name = go.name();
            let (patterns, gowork) = match &go.work {
                Some(work) => (go.patterns(), repo.join(work).display().to_string()),
                None => (Vec::new(), "off".to_owned()),
            };
            run_indexer(
                &cap,
                &out,
                &indexer,
                repo,
                &go.dir,
                &name,
                &patterns,
                &[("GOWORK", gowork)],
            )?;
            names.push(name);
        }
    }
    // The indexes of workspaces and modules that are gone (or no longer hold a
    // source file).
    for entry in std::fs::read_dir(&scip).map_err(|e| e.to_string())? {
        let entry = entry.map_err(|e| e.to_string())?;
        let file = entry.file_name().to_string_lossy().into_owned();
        if file.ends_with(".scip") && !names.contains(&file) {
            std::fs::remove_file(entry.path()).map_err(|e| e.to_string())?;
            eprintln!("removed {}", entry.path().display());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_in_base_1024() {
        assert_eq!(to_bytes("8G"), Some(8 * 1024 * 1024 * 1024));
        assert_eq!(to_bytes("512"), Some(512));
        assert_eq!(to_bytes("1.5k"), Some(1536));
        assert_eq!(to_bytes("8 GB"), None);
        assert_eq!(to_bytes("G"), None);
    }

    #[test]
    fn workspaces_and_their_index_names() {
        assert!(declares_workspace(
            "[package]\nname = \"a\"\n\n[workspace]\n"
        ));
        assert!(declares_workspace(
            "[workspace.package]\nedition = \"2024\"\n"
        ));
        assert!(!declares_workspace(
            "[package]\nname = \"a\"\n# [workspace]\n"
        ));
        assert_eq!(index_name(""), "root.scip");
        assert_eq!(
            index_name("duckdb-ext/code-db"),
            "ws-duckdb-ext+code-db.scip"
        );
        assert!(under("src/lib.rs", ""));
        assert!(under("tools/a/src/lib.rs", "tools/a"));
        assert!(!under("tools/ab/src/lib.rs", "tools/a"));
    }

    #[test]
    fn go_modules_and_their_index_names() {
        let files = [
            "cmd/main.go".to_owned(),
            "tools/gen/main.go".to_owned(),
            "src/lib.rs".to_owned(),
        ];
        let go_mods = [
            "go.mod".to_owned(),
            "tools/gen/go.mod".to_owned(),
            "docs/go.mod".to_owned(),
        ];
        assert_eq!(go_modules(&go_mods, &files), ["", "tools/gen"]);
        assert_eq!(go_modules(&go_mods, &files[2..]), Vec::<String>::new());
        assert_eq!(go_index_name(""), "go.scip");
        assert_eq!(go_index_name("tools/gen"), "go-tools+gen.scip");
    }

    #[test]
    fn work_uses_single_block_quoted_and_commented() {
        let text = "go 1.22\n\nuse ./a // the library\nuse (\n\t./b\n\t\"./c d\"\n\t// ./e\n\t../out\n)\nuser ./x\nreplace x => ./y\n";
        assert_eq!(work_uses(text), ["./a", "./b", "./c d", "../out"]);
        assert_eq!(work_uses("use (./a)\n"), ["./a"]);
        assert_eq!(work_uses("use(\n.\n)\n"), ["."]);
    }

    #[test]
    fn modules_a_go_work_uses_share_one_index() {
        let modules = [
            "".to_owned(),
            "lib".to_owned(),
            "svc".to_owned(),
            "svc/inner".to_owned(),
            "tools".to_owned(),
        ];
        let works = [
            (
                "go.work".to_owned(),
                "use (\n\t.\n\t./lib\n\t./svc/inner\n)\n".to_owned(),
            ),
            (
                "svc/go.work".to_owned(),
                "use .\nuse ./inner\nuse ../../outside\n".to_owned(),
            ),
        ];
        let indexes = go_indexes(&modules, &works);
        assert_eq!(
            indexes,
            [
                GoIndex {
                    dir: "".into(),
                    modules: vec!["".into(), "lib".into()],
                    work: Some("go.work".into()),
                },
                GoIndex {
                    dir: "svc".into(),
                    modules: vec!["svc".into(), "svc/inner".into()],
                    work: Some("svc/go.work".into()),
                },
                GoIndex {
                    dir: "tools".into(),
                    modules: vec!["tools".into()],
                    work: None,
                },
            ]
        );
        assert_eq!(indexes[0].patterns(), ["./...", "./lib/..."]);
        assert_eq!(indexes[1].patterns(), ["./...", "./inner/..."]);
        assert_eq!(
            indexes.iter().map(GoIndex::name).collect::<Vec<_>>(),
            ["gowork.scip", "gowork-svc.scip", "go-tools.scip"]
        );
    }
}
