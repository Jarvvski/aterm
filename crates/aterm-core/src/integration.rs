//! Shell-integration status: the visible three-state indicator (ticket T-2.6).
//!
//! The prototype's worst sin was *silent* zsh-only degradation: when integration
//! was missing it just showed a broken block UI with no explanation. The fix
//! (ADR-0008, [`04-shell-integration.md`] section 4 + recommendation 4) is to
//! surface a loud, always-honest three-state status with a "why":
//!
//! - **Integrated** - a nonce-matched OSC-133 `A` confirmed our hooks are live;
//!   command boundaries come from the marks and are authoritative.
//! - **Heuristic** - a supported shell (zsh/bash/fish) that did NOT confirm our
//!   marks. Either the shim could not be installed, or its hooks never fired
//!   (a hostile `.rc` clobbered them, an old/fragile bash tier, ...). We fall back
//!   to clearly-labeled *approximate* blocks ([`crate::block::HeuristicSegmenter`]).
//! - **None** - an unsupported shell (dash, ksh, nu, pwsh, ...). No shim, no blocks;
//!   a raw terminal view. We say so rather than pretending.
//!
//! The status is **observable and never silent**: it transitions from a brief
//! `Probing` (waiting for the first prompt mark) to either `Integrated` (on the
//! nonce-matched `A`) or, if the confirmation window elapses with no mark,
//! `Heuristic`. Every non-`Integrated` state carries a [`IntegrationReason`] whose
//! [`IntegrationReason::why`] is the one-click explanation the indicator shows.
//!
//! The decision logic lives in the pure [`IntegrationMonitor`] (no clock, no
//! threads), so it is exhaustively unit-tested with no PTY; the engine feeds it the
//! two facts it cannot know on its own - whether a nonce-matched `A` was seen
//! ([`IntegrationMonitor::confirm`]) and whether the confirmation window elapsed
//! ([`IntegrationMonitor::note_window_elapsed`]) - and publishes the resulting
//! [`Integration`] for the UI indicator ([`crate::engine::Engine::integration_status`]).

use crate::shell_integration::ShellKind;

/// The visible three-state integration status surfaced to the user (ADR-0008).
///
/// This is deliberately exactly three states - the indicator's glyph keys off it.
/// The nuance of *why* a session is `Heuristic`/`None` (and the brief pre-confirm
/// `Probing` sub-state) lives in the paired [`IntegrationReason`], so the indicator
/// can show a tooltip without exploding the state count.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationStatus {
    /// Confirmed: a nonce-matched OSC-133 `A` was seen. Marks are authoritative.
    Integrated,
    /// A supported shell with no confirmed marks; approximate (labeled) blocks.
    Heuristic,
    /// An unsupported shell: no integration, no blocks.
    None,
}

/// Why a session is in its [`IntegrationStatus`] - the "why?" the indicator shows.
///
/// Encoded as a small `u8` ([`IntegrationReason::code`]) so the engine can publish
/// the whole [`Integration`] through a single lock-free atomic from the model thread
/// to the UI handle (see [`Integration::code`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationReason {
    /// `Integrated`: the shim's marks were confirmed.
    Confirmed,
    /// `Heuristic` (transient): a supported shell whose shim is installed, but the
    /// first prompt mark has not arrived yet. We are still inside the confirmation
    /// window, so the heuristic detector is held off - real marks are expected
    /// imminently. Shown briefly right after spawn.
    Probing,
    /// `Heuristic`: the confirmation window elapsed with no nonce-matched mark, so
    /// the shell's hooks are not firing (clobbered `.rc`, a fragile bash 3.2 tier,
    /// or a nested un-integrated shell). Approximate blocks are now produced.
    HooksSilent,
    /// `Heuristic`: a supported shell, but the integration shim could not be
    /// installed this session (temp-dir creation failed, etc.). Approximate blocks.
    ShimInstallFailed,
    /// `None`: the shell is not one we know how to integrate.
    UnsupportedShell,
}

impl IntegrationReason {
    /// The one-click "why?" explanation, or `None` when integration is confirmed
    /// (there is nothing to explain). Generic by design: shell-version-specific
    /// wording (e.g. "bash 3.2 - upgrade for reliable blocks") is layered on by the
    /// UI, which knows the [`ShellKind`]; the version surfacing itself is a T-2.6
    /// follow-up (the bash tier is detected in-shell but not yet reported to Rust).
    #[must_use]
    pub fn why(self) -> Option<&'static str> {
        match self {
            IntegrationReason::Confirmed => None,
            IntegrationReason::Probing => Some("waiting for the shell's first prompt mark"),
            IntegrationReason::HooksSilent => {
                Some("shell-integration hooks did not load - showing approximate blocks")
            }
            IntegrationReason::ShimInstallFailed => {
                Some("could not install the shell-integration shim - showing approximate blocks")
            }
            IntegrationReason::UnsupportedShell => {
                Some("unsupported shell - no command-block integration")
            }
        }
    }

    /// Stable wire code for the atomic publish channel. Stable across versions so a
    /// stored value always decodes; new variants append.
    #[must_use]
    pub fn code(self) -> u8 {
        match self {
            IntegrationReason::Confirmed => 0,
            IntegrationReason::Probing => 1,
            IntegrationReason::HooksSilent => 2,
            IntegrationReason::ShimInstallFailed => 3,
            IntegrationReason::UnsupportedShell => 4,
        }
    }

    /// Decode a [`IntegrationReason::code`]. Unknown codes decode to `Probing` (the
    /// most benign "we do not know yet" state) rather than panicking.
    #[must_use]
    pub fn from_code(code: u8) -> Self {
        match code {
            0 => IntegrationReason::Confirmed,
            2 => IntegrationReason::HooksSilent,
            3 => IntegrationReason::ShimInstallFailed,
            4 => IntegrationReason::UnsupportedShell,
            _ => IntegrationReason::Probing,
        }
    }
}

/// The full integration state: the three-state [`IntegrationStatus`] plus the
/// [`IntegrationReason`] carrying its "why". `Copy` and atomic-encodable so the
/// engine publishes it lock-free from the model thread to the UI handle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Integration {
    pub status: IntegrationStatus,
    pub reason: IntegrationReason,
}

impl Integration {
    /// The "why?" explanation for the indicator, or `None` when `Integrated`.
    #[must_use]
    pub fn why(self) -> Option<&'static str> {
        self.reason.why()
    }

    /// Encode to a single `u8` for the engine's atomic publish slot. The reason
    /// determines the status one-to-one ([`IntegrationReason`] -> [`Integration`]),
    /// so the reason code is the whole encoding.
    #[must_use]
    pub fn code(self) -> u8 {
        self.reason.code()
    }

    /// Decode an [`Integration::code`] back to the full state.
    #[must_use]
    pub fn from_code(code: u8) -> Self {
        Self::from(IntegrationReason::from_code(code))
    }
}

impl From<IntegrationReason> for Integration {
    /// Map a reason to its status. This is the single source of truth for the
    /// reason -> status relationship.
    fn from(reason: IntegrationReason) -> Self {
        let status = match reason {
            IntegrationReason::Confirmed => IntegrationStatus::Integrated,
            IntegrationReason::Probing
            | IntegrationReason::HooksSilent
            | IntegrationReason::ShimInstallFailed => IntegrationStatus::Heuristic,
            IntegrationReason::UnsupportedShell => IntegrationStatus::None,
        };
        Self { status, reason }
    }
}

/// Pure decision machine for the integration indicator (ticket T-2.6).
///
/// Holds the few facts the status derives from and computes the current
/// [`Integration`]. No clock and no threads: the engine drives it with
/// [`Self::confirm`] (a nonce-matched `A` arrived) and [`Self::note_window_elapsed`]
/// (the confirmation window passed with no mark), so every transition is
/// deterministically testable.
#[derive(Debug, Clone, Copy)]
pub struct IntegrationMonitor {
    shell: ShellKind,
    /// Whether a nonce-armed shim was installed this session (vs. an install
    /// failure or an unsupported shell).
    shim_installed: bool,
    /// A nonce-matched OSC-133 `A` has been seen (hooks are live).
    confirmed: bool,
    /// The confirmation window elapsed without a mark (give up waiting; commit to
    /// the heuristic fallback).
    window_elapsed: bool,
}

impl IntegrationMonitor {
    /// A fresh monitor for a session on `shell`, where `shim_installed` says whether
    /// the nonce-armed integration shim was actually installed.
    #[must_use]
    pub fn new(shell: ShellKind, shim_installed: bool) -> Self {
        Self {
            shell,
            shim_installed,
            confirmed: false,
            window_elapsed: false,
        }
    }

    /// Record that a nonce-matched OSC-133 `A` was seen - integration is confirmed.
    /// Idempotent; later marks do not change anything.
    pub fn confirm(&mut self) {
        self.confirmed = true;
    }

    /// Record that the confirmation window elapsed with no nonce-matched mark. After
    /// this a supported, never-confirmed shell commits to `Heuristic`/`HooksSilent`.
    pub fn note_window_elapsed(&mut self) {
        self.window_elapsed = true;
    }

    /// Whether a nonce-matched `A` has confirmed integration.
    #[must_use]
    pub fn is_confirmed(&self) -> bool {
        self.confirmed
    }

    /// Whether a nonce-armed integration shim was installed this session. The engine
    /// only trusts (and confirms from) OSC-133 marks when this is true - the scanner
    /// is nonce-armed iff a shim loaded, so an untrusted scanner's marks (a forged
    /// `A` in command output) must never confirm integration.
    #[must_use]
    pub fn shim_installed(&self) -> bool {
        self.shim_installed
    }

    /// The current integration state + reason.
    #[must_use]
    pub fn integration(&self) -> Integration {
        let reason = if self.shell == ShellKind::Other {
            IntegrationReason::UnsupportedShell
        } else if self.confirmed {
            IntegrationReason::Confirmed
        } else if !self.shim_installed {
            IntegrationReason::ShimInstallFailed
        } else if self.window_elapsed {
            IntegrationReason::HooksSilent
        } else {
            IntegrationReason::Probing
        };
        Integration::from(reason)
    }

    /// The current three-state status (a thin accessor over [`Self::integration`]).
    #[must_use]
    pub fn status(&self) -> IntegrationStatus {
        self.integration().status
    }

    /// Whether the heuristic block detector should run right now. True only once we
    /// have concluded that nonce-matched marks are NOT coming (`HooksSilent` or
    /// `ShimInstallFailed`): during the brief `Probing` window we hold off (real
    /// marks are expected and would race the approximate blocks), and once
    /// `Integrated` the marks are authoritative so the heuristic must stay off.
    #[must_use]
    pub fn heuristic_active(&self) -> bool {
        matches!(
            self.integration().reason,
            IntegrationReason::HooksSilent | IntegrationReason::ShimInstallFailed
        )
    }

    /// Whether we are still waiting for the first mark (i.e. the engine should arm
    /// the confirmation-window timer). True only in the transient `Probing` state -
    /// a supported shell with a live shim that has neither confirmed nor timed out.
    #[must_use]
    pub fn awaiting_confirmation(&self) -> bool {
        self.integration().reason == IntegrationReason::Probing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn supported_with_shim() -> IntegrationMonitor {
        IntegrationMonitor::new(ShellKind::Zsh, true)
    }

    #[test]
    fn ac1_integrated_only_after_a_nonce_matched_a() {
        // AC1: a supported shell with a live shim shows Integrated ONLY after the
        // nonce-matched A confirms. Before that it is Heuristic/Probing, never
        // Integrated - the indicator must not claim integration it has not seen.
        let mut m = supported_with_shim();
        assert_eq!(m.status(), IntegrationStatus::Heuristic);
        assert_eq!(m.integration().reason, IntegrationReason::Probing);
        assert!(
            !m.heuristic_active(),
            "probing holds the heuristic detector off"
        );

        m.confirm();
        assert_eq!(m.status(), IntegrationStatus::Integrated);
        assert_eq!(m.integration().reason, IntegrationReason::Confirmed);
        assert!(m.integration().why().is_none(), "Integrated needs no why");
        assert!(!m.heuristic_active(), "confirmed marks are authoritative");
    }

    #[test]
    fn confirm_wins_over_a_later_window_elapse() {
        // Once confirmed, a (late) window-elapsed signal must not knock us back to
        // Heuristic - integration stays Integrated.
        let mut m = supported_with_shim();
        m.confirm();
        m.note_window_elapsed();
        assert_eq!(m.status(), IntegrationStatus::Integrated);
    }

    #[test]
    fn ac2_supported_but_silent_falls_back_to_labeled_heuristic() {
        // AC2: a supported shell whose hooks never fire (window elapses, no mark)
        // shows Heuristic and turns the approximate-block detector ON, with a why.
        let mut m = supported_with_shim();
        m.note_window_elapsed();
        assert_eq!(m.status(), IntegrationStatus::Heuristic);
        assert_eq!(m.integration().reason, IntegrationReason::HooksSilent);
        assert!(m.heuristic_active(), "silent hooks -> approximate blocks");
        assert!(m.integration().why().is_some(), "AC4: the why is populated");
    }

    #[test]
    fn supported_with_failed_shim_is_heuristic_immediately() {
        // A supported shell whose shim could not be installed is Heuristic from the
        // start (no point probing - there are no hooks to fire) and runs the
        // detector immediately, with its own distinct why.
        let m = IntegrationMonitor::new(ShellKind::Bash, false);
        assert_eq!(m.status(), IntegrationStatus::Heuristic);
        assert_eq!(m.integration().reason, IntegrationReason::ShimInstallFailed);
        assert!(m.heuristic_active());
        assert!(!m.awaiting_confirmation(), "no shim -> nothing to wait for");
        assert!(m.integration().why().is_some());
    }

    #[test]
    fn ac3_unsupported_shell_is_none() {
        // AC3: an unsupported shell (dash -> ShellKind::Other) is None - no blocks,
        // no heuristic, and it says why.
        for shim in [true, false] {
            let m = IntegrationMonitor::new(ShellKind::Other, shim);
            assert_eq!(m.status(), IntegrationStatus::None);
            assert_eq!(m.integration().reason, IntegrationReason::UnsupportedShell);
            assert!(
                !m.heuristic_active(),
                "unsupported shells show no blocks at all"
            );
            assert!(m.integration().why().is_some(), "AC4: the why is populated");
        }
    }

    #[test]
    fn ac4_every_non_integrated_reason_has_a_why() {
        // AC4: the why string is populated for EVERY non-Integrated case, and only
        // Confirmed (Integrated) has none.
        for reason in [
            IntegrationReason::Confirmed,
            IntegrationReason::Probing,
            IntegrationReason::HooksSilent,
            IntegrationReason::ShimInstallFailed,
            IntegrationReason::UnsupportedShell,
        ] {
            let has_why = reason.why().is_some();
            let integrated = Integration::from(reason).status == IntegrationStatus::Integrated;
            assert_eq!(
                has_why, !integrated,
                "exactly the non-Integrated reasons carry a why ({reason:?})"
            );
        }
    }

    #[test]
    fn ac5_status_transitions_are_observable_probing_then_settles() {
        // AC5: the status is observable and changes over the session lifetime -
        // Probing -> Integrated on a mark, or Probing -> Heuristic on a timeout.
        // Never a silent jump straight to a final state with no visible transition.
        let mut confirm_path = supported_with_shim();
        assert!(confirm_path.awaiting_confirmation());
        confirm_path.confirm();
        assert_eq!(confirm_path.status(), IntegrationStatus::Integrated);

        let mut timeout_path = supported_with_shim();
        assert!(timeout_path.awaiting_confirmation());
        timeout_path.note_window_elapsed();
        assert_eq!(timeout_path.status(), IntegrationStatus::Heuristic);
        assert!(
            !timeout_path.awaiting_confirmation(),
            "settled - timer disarmed"
        );
    }

    #[test]
    fn integration_round_trips_through_its_atomic_code() {
        // The engine publishes the whole Integration through one AtomicU8; every
        // reason must survive the encode/decode round-trip with its status intact.
        for reason in [
            IntegrationReason::Confirmed,
            IntegrationReason::Probing,
            IntegrationReason::HooksSilent,
            IntegrationReason::ShimInstallFailed,
            IntegrationReason::UnsupportedShell,
        ] {
            let original = Integration::from(reason);
            let decoded = Integration::from_code(original.code());
            assert_eq!(decoded, original, "round-trip for {reason:?}");
        }
        // Unknown codes decode to the benign Probing state, never panic.
        assert_eq!(
            Integration::from_code(200).reason,
            IntegrationReason::Probing
        );
    }
}
