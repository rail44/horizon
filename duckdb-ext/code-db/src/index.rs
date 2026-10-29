//! `code-db index`: jscpd's two reports and rust-analyzer's SCIP index, each
//! tool under a memory cap (duckdb-ext/sql/README.md, Limits).

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

pub fn index(root: &Path, repo: &Path, tool_memory_max: Option<&str>) -> Result<(), String> {
    let out = repo.join("target/code-db");
    std::fs::create_dir_all(&out).map_err(|e| e.to_string())?;
    let cap = choose_cap(tool_memory_max)?;

    let scan: Vec<String> = match std::env::var("JSCPD_PATHS") {
        Ok(paths) if !paths.is_empty() => paths.split_whitespace().map(str::to_owned).collect(),
        _ => ["src", "crates", "preview-plugin/src"]
            .into_iter()
            .filter(|d| repo.join(d).is_dir())
            .map(str::to_owned)
            .collect(),
    };
    if scan.is_empty() {
        return Err(format!(
            "no directory to scan in {} (set JSCPD_PATHS)",
            repo.display()
        ));
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
    let log = out.join("jscpd.log");
    let mut args = vec![
        repo.display().to_string(),
        out.join("jscpd").display().to_string(),
    ];
    args.extend(scan.iter().cloned());
    let jscpd = root.join("duckdb-ext/sql/jscpd.sh");
    let log_file = File::create(&log).map_err(|e| e.to_string())?;
    let status = run_logged(capped(&cap, &jscpd, &args), Stdio::null(), log_file)?;
    if !status.success() {
        return Err(failed("jscpd", status, &cap, &log));
    }
    eprintln!(
        "jscpd ({}): {:.1} s -> {} (log {})",
        scan.join(" "),
        start.elapsed().as_secs_f64(),
        out.join("jscpd").display(),
        log.display()
    );

    let ra = std::env::var("RUST_ANALYZER").unwrap_or_else(|_| "rust-analyzer".to_owned());
    let ra_path = PathBuf::from(&ra);
    if !(ra.contains('/') && ra_path.is_file() || on_path(&ra)) {
        return Err("rust-analyzer not found (set RUST_ANALYZER)".to_owned());
    }
    let start = Instant::now();
    let log = out.join("index.scip.log");
    let tmp = out.join("index.scip.tmp");
    let args = vec![
        "scip".to_owned(),
        ".".to_owned(),
        "--output".to_owned(),
        tmp.display().to_string(),
    ];
    let log_file = File::create(&log).map_err(|e| e.to_string())?;
    let stdout = Stdio::from(log_file.try_clone().map_err(|e| e.to_string())?);
    let mut command = capped(&cap, &ra_path, &args);
    command.current_dir(repo);
    let status = run_logged(command, stdout, log_file)?;
    if !status.success() {
        return Err(failed("rust-analyzer scip", status, &cap, &log));
    }
    let index = out.join("index.scip");
    std::fs::rename(&tmp, &index).map_err(|e| e.to_string())?;
    eprintln!(
        "rust-analyzer scip: {:.1} s -> {} (log {})",
        start.elapsed().as_secs_f64(),
        index.display(),
        log.display()
    );
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
}
