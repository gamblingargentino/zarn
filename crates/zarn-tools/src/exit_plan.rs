use anyhow::{Result, anyhow};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::Arc;
use zarn_core::tools::{Ctx, Risk, Tool, ToolOutput};
use zarn_core::types::ToolSpec;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlanVerdict {
    Approve(String),
    Reject(String),
}

#[async_trait]
pub trait PlanApprover: Send + Sync {
    async fn review(&self, plan: &str) -> PlanVerdict;
}

pub struct Unattended;

#[async_trait]
impl PlanApprover for Unattended {
    async fn review(&self, _plan: &str) -> PlanVerdict {
        PlanVerdict::Reject(
            "nobody is here to approve a plan; stay in plan mode and give the plan as your final answer".into(),
        )
    }
}

pub struct ExitPlan {
    approver: Arc<dyn PlanApprover>,
}

impl ExitPlan {
    pub fn new(approver: Arc<dyn PlanApprover>) -> Self {
        ExitPlan { approver }
    }
}

#[async_trait]
impl Tool for ExitPlan {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "exit_plan".to_string(),
            description: "In plan mode, present your finished plan to the user for approval. If they approve, the mode switches and you continue with the implementation; if they reject it, you stay in plan mode and get their feedback.".to_string(),
            schema: json!({
                "type": "object",
                "properties": {
                    "plan": { "type": "string", "description": "The plan, in Markdown, step by step" }
                },
                "required": ["plan"]
            }),
        }
    }

    fn risk(&self) -> Risk {
        Risk::Safe
    }

    async fn run(&self, args: Value, _ctx: &Ctx) -> Result<ToolOutput> {
        let plan = args
            .get("plan")
            .and_then(Value::as_str)
            .filter(|p| !p.trim().is_empty())
            .ok_or_else(|| anyhow!("exit_plan requires a non-empty plan"))?;
        Ok(match self.approver.review(plan).await {
            PlanVerdict::Approve(mode) => ToolOutput::ok(format!(
                "The user approved the plan. The mode is now {mode}. Carry out the plan."
            )),
            PlanVerdict::Reject(feedback) => ToolOutput::error(format!(
                "The user did not approve the plan; you are still in plan mode. Their feedback: {feedback}"
            )),
        })
    }
}
