//! [Fence](https://github.com/fencesandbox/fence) as the sandbox runtime:
//! bubblewrap on Linux, `sandbox-exec` on macOS, an HTTP and a SOCKS5 proxy
//! of its own per run. Each call gets a settings file of its own, passed
//! with `--settings`, so no `fence.json` of the workspace or the user is
//! read.

use super::{CONTAINED_ENV, Contained, Containment, RuntimeError, SandboxRuntime, Unavailable};
use serde_json::{Value, json};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Clone, Debug)]
pub struct FenceRuntime {
    binary: PathBuf,
}

impl FenceRuntime {
    pub fn new(binary: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
        }
    }

    pub fn binary(&self) -> &Path {
        &self.binary
    }
}

impl SandboxRuntime for FenceRuntime {
    fn name(&self) -> &'static str {
        "fence"
    }

    fn probe(&self) -> Result<(), Unavailable> {
        let ran = Command::new(&self.binary)
            .arg("--version")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map_err(|error| {
                Unavailable(format!(
                    "Fence could not be started from {}: {error}",
                    self.binary.display()
                ))
            })?;
        if !ran.success() {
            return Err(Unavailable(format!(
                "{} --version failed ({ran})",
                self.binary.display()
            )));
        }
        #[cfg(target_os = "linux")]
        for tool in ["bwrap", "socat"] {
            if which(tool).is_none() {
                return Err(Unavailable(format!(
                    "Fence needs `{tool}` on Linux, and it is not on PATH"
                )));
            }
        }
        Ok(())
    }

    fn contain(
        &self,
        script: &str,
        containment: &Containment,
        scratch: &Path,
    ) -> Result<Contained, RuntimeError> {
        let mut file = tempfile::Builder::new()
            .prefix("fence-")
            .suffix(".json")
            .tempfile_in(scratch)?;
        file.write_all(settings(containment).to_string().as_bytes())?;
        file.flush()?;
        let settings = file.into_temp_path();
        let mut command = Command::new(&self.binary);
        command
            .arg("--settings")
            .arg(&settings)
            .arg("-m")
            .arg("-c")
            .arg(script)
            .env(CONTAINED_ENV, self.name());
        Ok(Contained {
            command,
            keep_alive: vec![settings],
        })
    }

    fn refused_hosts(&self, stderr: &str) -> Vec<String> {
        let mut hosts: Vec<String> = Vec::new();
        for host in stderr.lines().filter_map(refused_host) {
            if !hosts.contains(&host) {
                hosts.push(host);
            }
        }
        hosts
    }
}

/// Fence's settings for one call. Fence's own command deny list is off: what
/// a command may do is decided before it runs, by the gate.
pub(super) fn settings(containment: &Containment) -> Value {
    let paths = |paths: &[PathBuf]| -> Vec<String> {
        paths
            .iter()
            .map(|path| path.display().to_string())
            .collect()
    };
    let mut network = json!({
        "allowedDomains": containment.domains,
        "deniedDomains": [],
    });
    if !containment.loopback_ports.is_empty() {
        network["allowLocalOutbound"] = json!(true);
        network["allowLocalOutboundPorts"] = json!(containment.loopback_ports);
    }
    let mut settings = json!({
        "network": network,
        "filesystem": {
            "allowWrite": paths(&containment.writable),
            "denyWrite": paths(&containment.read_only),
        },
        "command": { "useDefaults": false },
    });
    if !containment.mach_services.is_empty() {
        settings["macos"] = json!({ "mach": { "lookup": containment.mach_services } });
    }
    settings
}

/// The host of one of Fence's monitor lines for a refused connection:
/// `[fence:http] 11:08:14 ✗ CONNECT 403 example.com https://example.com:443 (0s)`
/// or `[fence:socks] 11:08:14 ✗ CONNECT example.org:443 BLOCKED`.
pub(super) fn refused_host(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("[fence:http]")
        .or_else(|| line.strip_prefix("[fence:socks]"))?;
    let mut tokens = rest.split_whitespace().skip_while(|token| *token != "✗");
    tokens.next()?; // ✗
    tokens.next()?; // the method
    let mut host = tokens.next()?;
    if host.chars().all(|c| c.is_ascii_digit()) {
        host = tokens.next()?; // the HTTP status came first
    }
    Some(strip_port(host).to_string())
}

fn strip_port(host: &str) -> &str {
    if let Some(bracketed) = host.strip_prefix('[') {
        return bracketed.split(']').next().unwrap_or(bracketed);
    }
    match host.rsplit_once(':') {
        Some((name, port)) if port.chars().all(|c| c.is_ascii_digit()) => name,
        _ => host,
    }
}

#[cfg(target_os = "linux")]
fn which(program: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(program))
        .find(|candidate| candidate.is_file())
}
