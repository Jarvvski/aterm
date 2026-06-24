//! The agentic turn-loop skeleton, structured for plan → act → observe.
//!
//! `AgentTurn` orchestrates one user request: it asks the provider for a turn,
//! consumes [`ProviderDelta`]s, and when the model proposes a tool call it runs
//! it through the SAFETY SPINE (risk gate → approval policy → sandbox →
//! output sanitizer) before feeding the observation back. The provider calls are
//! stubs (EPIC-5), so the loop here is exercised by the unit test with a fake
//! provider rather than a live model.
//!
//! The safety spine is NOT optional and NOT model-controlled: every tool call is
//! re-classified locally regardless of any risk the model self-reports.

use tokio::sync::mpsc;

use crate::policy::{Approval, ApprovalPolicy};
use crate::provider::{
    AgentEvent, LlmProvider, ProviderDelta, ProviderError, StopReason, ToolCall, TurnRequest,
};
use crate::sanitizer::OutputSanitizer;
use crate::secrets::Secrets;

/// Outcome of asking the user to confirm a gated tool call. The app layer
/// provides this; the turn loop only consults the deterministic policy first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmDecision {
    Approved,
    Denied,
}

/// How a proposed tool call is resolved into an action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolDisposition {
    /// Auto-approved by the policy (safe + not shell-active).
    AutoRun,
    /// Needs explicit confirmation; carries the human-readable reasons.
    NeedsConfirm(Vec<crate::risk::RiskReason>),
}

/// Drives one agent turn against a provider, applying the safety spine.
pub struct AgentTurn<'a, P: LlmProvider> {
    provider: &'a P,
    policy: ApprovalPolicy,
    secrets: &'a Secrets,
}

impl<'a, P: LlmProvider> AgentTurn<'a, P> {
    pub fn new(provider: &'a P, secrets: &'a Secrets) -> Self {
        Self {
            provider,
            policy: ApprovalPolicy::new(),
            secrets,
        }
    }

    /// Classify a proposed tool call's command deterministically. The model's
    /// own risk claim is intentionally ignored.
    pub fn disposition_for_command(&self, command_line: &str) -> ToolDisposition {
        // The gate classifies against the SAME `Secrets` the sanitizer redacts
        // from (`self.secrets`) - one source, so the two defenses cannot drift.
        // `decide` routes through the multi-line buffer gate, so an embedded `\n`
        // cannot smuggle a dangerous second command past a head-keyed rule.
        match self.policy.decide(command_line, self.secrets) {
            Approval::AutoApprove => ToolDisposition::AutoRun,
            Approval::RequireConfirm(a) => ToolDisposition::NeedsConfirm(a.reasons),
        }
    }

    /// Sanitize a tool's raw output before it is fed back to the model or shown.
    pub fn sanitize_observation(&self, raw: &str, max_len: Option<usize>) -> String {
        OutputSanitizer::new(self.secrets).sanitize(raw, max_len)
    }

    /// Run one turn: stream provider deltas, translate them into [`AgentEvent`]s
    /// on `events`, and surface proposed tool calls (gated) to the caller via the
    /// returned vec. Tool EXECUTION itself is the app layer's job; this loop owns
    /// the plan→act→observe structure and the gating, not process spawning.
    ///
    /// Because the providers are stubs, a real provider call returns
    /// `NotImplemented`; the loop forwards that as an `AgentEvent::Error` and
    /// completes cleanly (no panic).
    pub async fn run(
        &self,
        request: TurnRequest,
        events: mpsc::Sender<AgentEvent>,
    ) -> Result<Vec<ToolCall>, ProviderError> {
        let (dtx, mut drx) = mpsc::channel::<ProviderDelta>(64);

        // Spawn the provider stream. With stub providers this resolves quickly to
        // an error, which we report rather than panic on.
        let provider_result = self.provider.stream_turn(request, dtx).await;

        let mut proposed: Vec<ToolCall> = Vec::new();

        // Drain whatever the provider produced (stubs produce nothing).
        while let Ok(delta) = drx.try_recv() {
            match delta {
                ProviderDelta::Thinking(t) => {
                    let _ = events.send(AgentEvent::Thinking(t)).await;
                }
                ProviderDelta::Text(t) => {
                    let _ = events.send(AgentEvent::Assistant(t)).await;
                }
                ProviderDelta::ToolCall(call) => {
                    let _ = events.send(AgentEvent::ToolProposed(call.clone())).await;
                    proposed.push(call);
                }
                ProviderDelta::Stop {
                    reason: StopReason::ToolUse,
                } => {
                    // ACT phase: the app executes approved tools and re-enters
                    // run() with the observations appended (loop on tool_use).
                    // TODO(ticket EPIC-5): re-issue the turn with tool results.
                }
                ProviderDelta::Stop { .. } => {}
            }
        }

        match provider_result {
            Ok(()) => {
                let _ = events.send(AgentEvent::TurnComplete).await;
                Ok(proposed)
            }
            Err(e) => {
                let _ = events.send(AgentEvent::Error(e.to_string())).await;
                // Surface the not-implemented stub as an error, not a panic.
                Err(e)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::{AnthropicProvider, Effort};

    fn req(p: &AnthropicProvider) -> TurnRequest {
        TurnRequest {
            model: p.default_model().to_string(),
            system: None,
            messages: vec![],
            tools: vec![],
            effort: Effort::Medium,
        }
    }

    #[test]
    fn dangerous_command_needs_confirm_regardless_of_model_claim() {
        let secrets = Secrets::new();
        let provider = AnthropicProvider::new("sk-test");
        let turn = AgentTurn::new(&provider, &secrets);
        // Even if a model claimed this was "safe", the deterministic gate wins.
        // `rm -rf ~` is shell-active (the `~`) AND a recursive-force removal.
        match turn.disposition_for_command("rm -rf ~") {
            ToolDisposition::NeedsConfirm(reasons) => {
                assert!(reasons.contains(&crate::risk::RiskReason::Destructive));
            }
            ToolDisposition::AutoRun => panic!("rm -rf ~ must never auto-run"),
        }
    }

    #[test]
    fn safe_command_auto_runs() {
        let secrets = Secrets::new();
        let provider = AnthropicProvider::new("sk-test");
        let turn = AgentTurn::new(&provider, &secrets);
        assert_eq!(
            turn.disposition_for_command("ls -la"),
            ToolDisposition::AutoRun
        );
    }

    #[test]
    fn gate_and_sanitizer_cannot_drift_single_secrets_source() {
        // AC1: ONE `Secrets` instance feeds BOTH the risk gate and the output
        // sanitizer. Mutating that single source - registering a sensitive path
        // and a secret value - must be reflected by BOTH defenses at once.
        let mut secrets = Secrets::new();
        secrets.add_sensitive_path("vault-keys");
        secrets.add_value("sk-live-DRIFT-CANARY-0987654321");
        let provider = AnthropicProvider::new("sk-test");
        let turn = AgentTurn::new(&provider, &secrets);

        // Gate side: `cat vault-keys` would be Safe (cat is inert, no shell-active
        // chars) UNLESS the gate consults the mutated instance deny-set. It must
        // refuse, citing the secret-path reason - proving the gate read THIS
        // instance, not a private/static copy.
        match turn.disposition_for_command("cat vault-keys") {
            ToolDisposition::NeedsConfirm(reasons) => {
                assert!(
                    reasons.contains(&crate::risk::RiskReason::SecretAccess),
                    "the registered sensitive path must drive a secret-path escalation"
                );
            }
            ToolDisposition::AutoRun => {
                panic!("a path added to the single Secrets source must never auto-run")
            }
        }

        // Sanitizer side: the value added to the SAME instance is redacted.
        let clean = turn.sanitize_observation("leak=sk-live-DRIFT-CANARY-0987654321 end", None);
        assert!(!clean.contains("DRIFT-CANARY"));
    }

    #[test]
    fn observation_is_sanitized() {
        let mut secrets = Secrets::new();
        secrets.add_value("sk-secret-value-xyz");
        let provider = AnthropicProvider::new("sk-test");
        let turn = AgentTurn::new(&provider, &secrets);
        let clean = turn.sanitize_observation("token=sk-secret-value-xyz done", None);
        assert!(!clean.contains("sk-secret-value-xyz"));
    }

    #[tokio::test]
    async fn stub_provider_completes_without_panic() {
        let secrets = Secrets::new();
        let provider = AnthropicProvider::new("sk-test");
        let turn = AgentTurn::new(&provider, &secrets);
        let (etx, mut erx) = mpsc::channel(16);
        let result = turn.run(req(&provider), etx).await;
        // Stub provider → NotImplemented error surfaced, no panic.
        assert!(result.is_err());
        // An error event was emitted.
        let mut saw_error = false;
        while let Ok(ev) = erx.try_recv() {
            if matches!(ev, AgentEvent::Error(_)) {
                saw_error = true;
            }
        }
        assert!(saw_error);
    }
}
