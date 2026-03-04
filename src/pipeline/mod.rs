//! Task pipeline — transforms raw prompts into structured agent execution.
//!
//! Stages: refine → context → plan → dispatch → verify.
//! Each stage produces an artifact (TASK.md, CONTEXT.md, PLAN.md) that is
//! inspectable, diffable, and reusable.

pub mod artifacts;
pub mod context;
pub mod dispatch;
pub mod plan;
pub mod refine;
pub mod verify;

use crate::worker::Worker;
use color_eyre::eyre::Result;
use std::path::Path;

/// Pipeline stage identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PipelineStage {
    Raw,
    Refined,
    Contextualized,
    Planned,
    Dispatched,
    Verified,
    #[allow(dead_code)]
    Failed,
}

impl PipelineStage {
    /// Parse from a CLI string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "raw" => Some(Self::Raw),
            "refine" | "refined" => Some(Self::Refined),
            "context" | "contextualized" => Some(Self::Contextualized),
            "plan" | "planned" => Some(Self::Planned),
            "dispatch" | "dispatched" => Some(Self::Dispatched),
            "verify" | "verified" => Some(Self::Verified),
            _ => None,
        }
    }
}

impl std::fmt::Display for PipelineStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Raw => write!(f, "raw"),
            Self::Refined => write!(f, "refined"),
            Self::Contextualized => write!(f, "contextualized"),
            Self::Planned => write!(f, "planned"),
            Self::Dispatched => write!(f, "dispatched"),
            Self::Verified => write!(f, "verified"),
            Self::Failed => write!(f, "failed"),
        }
    }
}

/// Options for running the full pipeline.
pub struct PipelineOptions<'a> {
    pub raw_prompt: &'a str,
    pub workspace_root: &'a Path,
    pub conventions: Option<&'a str>,
    pub repo: Option<&'a str>,
    pub profile: &'a str,
    pub until: Option<PipelineStage>,
    pub dry_run: bool,
}

/// Result of a pipeline run.
pub struct PipelineResult {
    pub task_md: String,
    pub context_md: Option<String>,
    pub plan_md: Option<String>,
    pub worktree_id: Option<String>,
    pub stage_reached: PipelineStage,
}

/// Run the pipeline: refine → context → plan → dispatch.
///
/// Stops early if `until` is set or `dry_run` is true.
pub async fn run_pipeline(opts: PipelineOptions<'_>) -> Result<PipelineResult> {
    // Stage 1: Refine
    eprintln!("[pipeline] Stage 1/4: Refine");
    let refined = refine::refine(opts.raw_prompt, opts.conventions).await?;
    eprintln!("[pipeline] Refined: \"{}\"", refined.title);

    if opts.until == Some(PipelineStage::Refined) {
        return Ok(PipelineResult {
            task_md: refined.task_md,
            context_md: None,
            plan_md: None,
            worktree_id: None,
            stage_reached: PipelineStage::Refined,
        });
    }

    // Stage 2: Context
    eprintln!("[pipeline] Stage 2/4: Context");
    let ctx = context::identify_context(&refined.task_md, Some(opts.workspace_root)).await?;
    eprintln!(
        "[pipeline] Context: {} relevant files",
        ctx.relevant_files.len()
    );

    if opts.until == Some(PipelineStage::Contextualized) {
        return Ok(PipelineResult {
            task_md: refined.task_md,
            context_md: Some(ctx.context_md),
            plan_md: None,
            worktree_id: None,
            stage_reached: PipelineStage::Contextualized,
        });
    }

    // Stage 3: Plan
    eprintln!("[pipeline] Stage 3/4: Plan");
    let planned = plan::create_plan(&refined.task_md, &ctx.context_md).await?;
    eprintln!("[pipeline] Plan: {} steps", planned.steps.len());

    if opts.until == Some(PipelineStage::Planned) || opts.dry_run {
        return Ok(PipelineResult {
            task_md: refined.task_md,
            context_md: Some(ctx.context_md),
            plan_md: Some(planned.plan_md),
            worktree_id: None,
            stage_reached: PipelineStage::Planned,
        });
    }

    // Stage 4: Dispatch
    eprintln!("[pipeline] Stage 4/4: Dispatch");
    let prompt = dispatch::build_dispatch_prompt(&refined.title);
    let worker = Worker::new();
    let worktree_id = worker
        .dispatch_pipeline_task(
            opts.workspace_root,
            &prompt,
            opts.repo,
            opts.profile,
            &refined.task_md,
            Some(&ctx.context_md),
            Some(&planned.plan_md),
        )
        .await?;

    eprintln!("[pipeline] Dispatched: {worktree_id}");

    Ok(PipelineResult {
        task_md: refined.task_md,
        context_md: Some(ctx.context_md),
        plan_md: Some(planned.plan_md),
        worktree_id: Some(worktree_id),
        stage_reached: PipelineStage::Dispatched,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pipeline_stage_from_str() {
        assert_eq!(
            PipelineStage::from_str("refine"),
            Some(PipelineStage::Refined)
        );
        assert_eq!(
            PipelineStage::from_str("refined"),
            Some(PipelineStage::Refined)
        );
        assert_eq!(
            PipelineStage::from_str("context"),
            Some(PipelineStage::Contextualized)
        );
        assert_eq!(
            PipelineStage::from_str("plan"),
            Some(PipelineStage::Planned)
        );
        assert_eq!(
            PipelineStage::from_str("dispatch"),
            Some(PipelineStage::Dispatched)
        );
        assert_eq!(
            PipelineStage::from_str("verify"),
            Some(PipelineStage::Verified)
        );
        assert_eq!(PipelineStage::from_str("nope"), None);
    }

    #[test]
    fn pipeline_stage_display() {
        assert_eq!(PipelineStage::Raw.to_string(), "raw");
        assert_eq!(PipelineStage::Refined.to_string(), "refined");
        assert_eq!(PipelineStage::Failed.to_string(), "failed");
    }
}
