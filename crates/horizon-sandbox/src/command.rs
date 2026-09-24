//! Command context shared by the OS-specific sandbox wrappers.
use crate::{SandboxError, SandboxPolicy, SandboxStdio};
use std::process::Command;

pub(crate) fn configure_child(
    command: &Command,
    wrapped: &mut Command,
    policy: &SandboxPolicy,
    stdio: SandboxStdio,
) -> Result<(), SandboxError> {
    if let Some(cwd) = command.get_current_dir() {
        wrapped.current_dir(cwd);
    }
    for (key, value) in command.get_envs() {
        match value {
            Some(value) => {
                wrapped.env(key, value);
            }
            None => {
                wrapped.env_remove(key);
            }
        }
    }
    crate::tmpdir::provision(policy, command, wrapped)?;
    wrapped
        .stdin(stdio.stdin)
        .stdout(stdio.stdout)
        .stderr(stdio.stderr);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{NetworkPolicy, ReadableScope};
    use std::io::Write;
    use std::process::Stdio;

    #[test]
    fn wrapper_forwards_context_and_all_three_streams() {
        let root = std::env::temp_dir().join(format!("horizon-command-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let root = root.canonicalize().unwrap();
        let policy = SandboxPolicy {
            writable_roots: vec![root.clone()],
            readable_scope: ReadableScope::Full,
            network: NetworkPolicy::Disabled,
        };
        let mut original = Command::new("/bin/sh");
        original
            .current_dir(&root)
            .env("HORIZON_TEST_VALUE", "value with spaces")
            .env_remove("HORIZON_TEST_REMOVED")
            .env_remove("TMPDIR");
        let mut wrapped = Command::new("/bin/sh");
        wrapped.env("HORIZON_TEST_REMOVED", "must be removed").arg("-c").arg(
            "read line; printf '%s\\n' \"$PWD\" \"$HORIZON_TEST_VALUE\" \"${HORIZON_TEST_REMOVED-unset}\" \"$TMPDIR\" \"$line\"; printf 'stderr bytes' >&2"
        );
        configure_child(
            &original,
            &mut wrapped,
            &policy,
            SandboxStdio {
                stdin: Stdio::piped(),
                stdout: Stdio::piped(),
                stderr: Stdio::piped(),
            },
        )
        .unwrap();
        let mut child = wrapped.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(b"stdin bytes\n")
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!(
                "{}\nvalue with spaces\nunset\n{}\nstdin bytes\n",
                root.display(),
                root.join(crate::SCRATCH_DIR_NAME).display()
            )
        );
        assert_eq!(output.stderr, b"stderr bytes");
        assert!(root.join(crate::SCRATCH_DIR_NAME).is_dir());
        std::fs::remove_dir_all(root).unwrap();
    }
}
