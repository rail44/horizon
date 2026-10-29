//! `code-db index`: jscpd's two reports over the source files and one
//! rust-analyzer SCIP index per Cargo workspace, each tool under a memory cap
//! (duckdb-ext/sql/README.md, Running and Limits).

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
/// the settings' scope applied) and the Cargo workspaces to index, as
/// repository-relative paths; a workspace is a directory whose tracked
/// Cargo.toml has a `[workspace]` table and that holds at least one source
/// file ("" for the repository root).
pub struct Inputs {
    pub files: Vec<String>,
    pub workspaces: Vec<String>,
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

    let ra = std::env::var("RUST_ANALYZER").unwrap_or_else(|_| "rust-analyzer".to_owned());
    let ra_path = PathBuf::from(&ra);
    if !(ra.contains('/') && ra_path.is_file() || on_path(&ra)) {
        return Err("rust-analyzer not found (set RUST_ANALYZER)".to_owned());
    }
    let scip = out.join("scip");
    std::fs::create_dir_all(&scip).map_err(|e| e.to_string())?;
    let names: Vec<String> = inputs.workspaces.iter().map(|w| index_name(w)).collect();
    for (workspace, name) in inputs.workspaces.iter().zip(&names) {
        let start = Instant::now();
        let log = logs.join(format!("{name}.log"));
        let tmp = scip.join(format!("{name}.tmp"));
        let args = vec![
            "scip".to_owned(),
            ".".to_owned(),
            "--output".to_owned(),
            tmp.display().to_string(),
        ];
        let log_file = File::create(&log).map_err(|e| e.to_string())?;
        let stdout = Stdio::from(log_file.try_clone().map_err(|e| e.to_string())?);
        let mut command = capped(&cap, &ra_path, &args);
        command.current_dir(repo.join(workspace));
        let status = run_logged(command, stdout, log_file)?;
        let shown = if workspace.is_empty() { "." } else { workspace };
        if !status.success() {
            return Err(failed(
                &format!("rust-analyzer scip in {shown}"),
                status,
                &cap,
                &log,
            ));
        }
        let index = scip.join(name);
        std::fs::rename(&tmp, &index).map_err(|e| e.to_string())?;
        eprintln!(
            "rust-analyzer scip ({shown}): {:.1} s -> {} (log {})",
            start.elapsed().as_secs_f64(),
            index.display(),
            log.display()
        );
    }
    // The indexes of workspaces that are gone (or no longer hold a source file).
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
}
