//! The approval seam (ticket T-5.11): the channel that connects the deterministic
//! gate's `RequireConfirm` verdict to a human Approve/Deny decision.
//!
//! The turn loop (T-5.8) consults the deterministic [`ApprovalPolicy`](crate::ApprovalPolicy)
//! first and only calls a [`ConfirmHandler`](crate::ConfirmHandler) when the policy
//! demands confirmation. [`ChannelConfirmHandler`] is the handler the app installs:
//! each gated call is surfaced as an [`ApprovalRequest`] on a channel (the seam a UI
//! Approve/Deny button - or an Esc deny - feeds), and the loop BLOCKS on
//! `confirm().await` until the reply arrives. This keeps the GPU/UI thread out of the
//! agent runtime: the renderer draws the badge + reasons (T-5.11), and a click sends
//! the decision back over the reply channel.
//!
//! FAIL-CLOSED is the safety stance: if the UI receiver is gone, or the reply channel
//! is dropped without a decision, the call is DENIED. An escalation the user never
//! confirmed must never run.

use tokio::sync::{mpsc, oneshot};

use crate::provider::ToolCall;
use crate::risk::RiskAssessment;
use crate::turn::{ConfirmDecision, ConfirmHandler};

/// The default depth of the pending-approval channel. Approvals are rare and
/// serialized by the human, so a small buffer is ample; backpressure here simply
/// parks the turn loop, which is the desired "wait for the human" behavior.
const APPROVAL_CHANNEL_DEPTH: usize = 16;

/// One pending approval surfaced to the UI: the gated [`ToolCall`], the deterministic
/// [`RiskAssessment`] that escalated it (its level + reasons drive the badge + gloss),
/// and the `reply` channel the UI answers on. The renderer shows the proposal; an
/// Approve/Deny click (or Esc = deny) calls [`approve`](ApprovalRequest::approve) /
/// [`deny`](ApprovalRequest::deny).
#[derive(Debug)]
pub struct ApprovalRequest {
    pub call: ToolCall,
    pub assessment: RiskAssessment,
    reply: oneshot::Sender<ConfirmDecision>,
}

impl ApprovalRequest {
    /// Resolve this request with the user's decision. Consumes the request; if the
    /// loop already gave up waiting (its receiver dropped), the send is a no-op.
    pub fn resolve(self, decision: ConfirmDecision) {
        let _ = self.reply.send(decision);
    }

    /// Approve the gated call (it will run).
    pub fn approve(self) {
        self.resolve(ConfirmDecision::Approved);
    }

    /// Deny the gated call (it is fed back as an `is_error` result, never run). This
    /// is also the Esc-during-approval action.
    pub fn deny(self) {
        self.resolve(ConfirmDecision::Denied);
    }
}

/// A [`ConfirmHandler`] whose decision is delivered out-of-band over a channel - the
/// seam between the deterministic gate and the human. The turn loop BLOCKS on
/// `confirm().await` until the UI answers the surfaced [`ApprovalRequest`].
///
/// FAIL-CLOSED: a closed UI channel, or a dropped reply, resolves to
/// [`ConfirmDecision::Denied`] - an unconfirmed escalation never runs.
pub struct ChannelConfirmHandler {
    tx: mpsc::Sender<ApprovalRequest>,
}

impl ChannelConfirmHandler {
    /// Build a handler plus the receiver the UI drains for pending approvals. Install
    /// the handler in the turn loop; poll the receiver on the UI side and answer each
    /// [`ApprovalRequest`].
    #[must_use]
    pub fn new() -> (Self, mpsc::Receiver<ApprovalRequest>) {
        let (tx, rx) = mpsc::channel(APPROVAL_CHANNEL_DEPTH);
        (Self { tx }, rx)
    }
}

impl ConfirmHandler for ChannelConfirmHandler {
    async fn confirm(&self, call: &ToolCall, assessment: &RiskAssessment) -> ConfirmDecision {
        let (reply, wait) = oneshot::channel();
        let req = ApprovalRequest {
            call: call.clone(),
            assessment: assessment.clone(),
            reply,
        };
        // The UI receiver is gone -> fail closed (deny, never auto-run).
        if self.tx.send(req).await.is_err() {
            return ConfirmDecision::Denied;
        }
        // The reply was dropped without a decision -> fail closed.
        wait.await.unwrap_or(ConfirmDecision::Denied)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::risk::{Risk, RiskReason};
    use serde_json::json;

    fn caution_call() -> (ToolCall, RiskAssessment) {
        (
            ToolCall {
                id: "toolu_1".into(),
                name: "run_command".into(),
                input: json!({"command": ["brew", "install", "wget"]}),
            },
            RiskAssessment {
                level: Risk::Caution,
                reasons: vec![RiskReason::PackageMutator],
            },
        )
    }

    #[tokio::test]
    async fn confirm_blocks_until_the_ui_answers_then_returns_the_decision() {
        let (handler, mut rx) = ChannelConfirmHandler::new();
        let (call, assessment) = caution_call();

        // The loop side parks on confirm().await.
        let confirming = tokio::spawn(async move { handler.confirm(&call, &assessment).await });

        // The request surfaces to the UI with the call + the escalating assessment.
        let req = rx.recv().await.expect("a request must surface");
        assert_eq!(req.call.id, "toolu_1");
        assert_eq!(req.assessment.level, Risk::Caution);
        // It has NOT resolved yet - the loop is genuinely blocked on the human.
        assert!(!confirming.is_finished());

        // The UI approves; the parked confirm() now resolves to Approved.
        req.approve();
        assert_eq!(confirming.await.unwrap(), ConfirmDecision::Approved);
    }

    #[tokio::test]
    async fn deny_resolves_the_parked_confirm_to_denied() {
        let (handler, mut rx) = ChannelConfirmHandler::new();
        let (call, assessment) = caution_call();
        let confirming = tokio::spawn(async move { handler.confirm(&call, &assessment).await });
        let req = rx.recv().await.unwrap();
        req.deny();
        assert_eq!(confirming.await.unwrap(), ConfirmDecision::Denied);
    }

    #[tokio::test]
    async fn a_dropped_reply_fails_closed_to_denied() {
        // The UI surfaced the request but dropped it without deciding (e.g. the
        // approval card was dismissed): the call must be DENIED, never run.
        let (handler, mut rx) = ChannelConfirmHandler::new();
        let (call, assessment) = caution_call();
        let confirming = tokio::spawn(async move { handler.confirm(&call, &assessment).await });
        let req = rx.recv().await.unwrap();
        drop(req); // no decision
        assert_eq!(confirming.await.unwrap(), ConfirmDecision::Denied);
    }

    #[tokio::test]
    async fn a_closed_ui_channel_fails_closed_to_denied() {
        // The UI side is gone before the call is even surfaced: fail closed.
        let (handler, rx) = ChannelConfirmHandler::new();
        drop(rx);
        let (call, assessment) = caution_call();
        assert_eq!(
            handler.confirm(&call, &assessment).await,
            ConfirmDecision::Denied
        );
    }
}
