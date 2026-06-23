//! `ApprovalPolicy`: AUTO-SAFE default. A command is auto-approved ONLY if it
//! classifies [`Risk::Safe`] AND carries no shell-active reason. Everything at
//! [`Risk::Caution`] or [`Risk::Dangerous`] always requires explicit
//! confirmation. The model's opinion never enters this decision.

use crate::command::ShellCommand;
use crate::risk::{DefaultRiskClassifier, Risk, RiskAssessment, RiskReason};

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

/// The approval policy. Wraps a risk classifier.
#[derive(Debug, Clone, Default)]
pub struct ApprovalPolicy {
    classifier: DefaultRiskClassifier,
}

impl ApprovalPolicy {
    pub fn new() -> Self {
        Self::default()
    }

    /// Decide on a raw command line.
    pub fn decide(&self, line: &str) -> Approval {
        self.decide_command(&ShellCommand::parse(line))
    }

    /// Decide on a parsed command.
    pub fn decide_command(&self, cmd: &ShellCommand) -> Approval {
        let assessment = self.classifier.classify(cmd);
        // AUTO-SAFE: only Safe AND not shell-active.
        let shell_active = cmd.structure.is_shell_active()
            || assessment.reasons.contains(&RiskReason::ShellActive);
        if assessment.level == Risk::Safe && !shell_active {
            Approval::AutoApprove
        } else {
            Approval::RequireConfirm(assessment)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_inert_command_auto_approves() {
        let p = ApprovalPolicy::new();
        assert!(p.decide("ls -la").is_auto());
        assert!(p.decide("git status").is_auto());
    }

    #[test]
    fn caution_requires_confirm() {
        let p = ApprovalPolicy::new();
        assert!(!p.decide("brew list").is_auto());
        assert!(!p.decide("some-random-binary").is_auto());
    }

    #[test]
    fn dangerous_requires_confirm() {
        let p = ApprovalPolicy::new();
        match p.decide("rm -rf ~") {
            Approval::RequireConfirm(a) => {
                assert_eq!(a.level, Risk::Dangerous);
            }
            _ => panic!("rm -rf must require confirmation"),
        }
    }

    #[test]
    fn shell_active_safe_program_still_requires_confirm() {
        // `ls | grep` is built from safe programs but is shell-active.
        let p = ApprovalPolicy::new();
        assert!(!p.decide("ls | grep foo").is_auto());
    }
}
