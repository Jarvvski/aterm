//! `Sandbox`: the execution-confinement seam. A `Dangerous`/`Caution` command
//! that the user approves can still be run inside a sandbox that limits what it
//! may touch (filesystem writes, network). On macOS this wraps `sandbox-exec`
//! (Seatbelt); elsewhere / when disabled we fall back to [`NoSandbox`].
//!
//! The trait and signatures are real; the Seatbelt body is a documented stub.
//! TODO(ticket EPIC-5): generate a real `.sb` profile from the policy and the
//! command's declared paths, then exec under it.

use std::path::PathBuf;

/// How permissive the sandbox should be for one command. Default denies network
/// and grants no writable paths (most restrictive).
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    /// Paths the command may write to (everything else is read-only / denied).
    pub writable_paths: Vec<PathBuf>,
    /// Whether outbound network is permitted.
    pub allow_network: bool,
}

/// A command ready to be confined: program + args + cwd.
#[derive(Debug, Clone)]
pub struct ConfinedCommand {
    pub program: String,
    pub args: Vec<String>,
    pub cwd: Option<PathBuf>,
}

/// Errors from sandbox setup.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error("sandbox not yet implemented: {0}")]
    NotImplemented(&'static str),
    #[error("invalid sandbox policy: {0}")]
    InvalidPolicy(String),
}

/// The confinement seam. Implementations rewrite a [`ConfinedCommand`] into the
/// argv that actually runs (e.g. `sandbox-exec -p <profile> -- <cmd>`).
pub trait Sandbox: Send + Sync {
    /// Human-readable backend name (for logging / UI).
    fn name(&self) -> &'static str;

    /// Whether this backend actually confines anything.
    fn is_enforcing(&self) -> bool;

    /// Produce the final argv to spawn for `cmd` under `policy`. The first
    /// element is the program to exec.
    fn wrap(
        &self,
        cmd: &ConfinedCommand,
        policy: &SandboxPolicy,
    ) -> Result<Vec<String>, SandboxError>;
}

/// macOS Seatbelt sandbox via `sandbox-exec`. STUB.
#[derive(Debug, Clone, Default)]
pub struct SeatbeltSandbox;

impl Sandbox for SeatbeltSandbox {
    fn name(&self) -> &'static str {
        "seatbelt"
    }

    fn is_enforcing(&self) -> bool {
        true
    }

    fn wrap(
        &self,
        _cmd: &ConfinedCommand,
        _policy: &SandboxPolicy,
    ) -> Result<Vec<String>, SandboxError> {
        // TODO(ticket EPIC-5): build a Seatbelt `.sb` profile from `policy`
        // (deny default, allow file-read*, allow file-write* for writable_paths,
        // (allow|deny) network*) and return:
        //   ["sandbox-exec", "-p", <profile>, "--", program, args...]
        Err(SandboxError::NotImplemented(
            "SeatbeltSandbox::wrap — Seatbelt profile generation is EPIC-5",
        ))
    }
}

/// No-op fallback: runs the command unconfined.
#[derive(Debug, Clone, Default)]
pub struct NoSandbox;

impl Sandbox for NoSandbox {
    fn name(&self) -> &'static str {
        "none"
    }

    fn is_enforcing(&self) -> bool {
        false
    }

    fn wrap(
        &self,
        cmd: &ConfinedCommand,
        _policy: &SandboxPolicy,
    ) -> Result<Vec<String>, SandboxError> {
        let mut argv = Vec::with_capacity(cmd.args.len() + 1);
        argv.push(cmd.program.clone());
        argv.extend(cmd.args.iter().cloned());
        Ok(argv)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd() -> ConfinedCommand {
        ConfinedCommand {
            program: "ls".into(),
            args: vec!["-la".into()],
            cwd: None,
        }
    }

    #[test]
    fn no_sandbox_passes_command_through() {
        let argv = NoSandbox.wrap(&cmd(), &SandboxPolicy::default()).unwrap();
        assert_eq!(argv, vec!["ls", "-la"]);
        assert!(!NoSandbox.is_enforcing());
    }

    #[test]
    fn seatbelt_is_stub_but_reports_enforcing() {
        let sb = SeatbeltSandbox;
        assert!(sb.is_enforcing());
        assert_eq!(sb.name(), "seatbelt");
        assert!(matches!(
            sb.wrap(&cmd(), &SandboxPolicy::default()),
            Err(SandboxError::NotImplemented(_))
        ));
    }
}
