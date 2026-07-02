use super::{Registry, Tool, ToolContext, ToolOutput};
use crate::agent::Agent;
use crate::logging;
use crate::provider::Provider;
use crate::session::Session;
use anyhow::Result;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{Value, json};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Diff capture per attempt is truncated to this many chars (tail-safe: keeps
/// the most recent part of the diff, matching the memory-rerank truncation
/// pattern used elsewhere in the codebase).
const MAX_DIFF_CHARS: usize = 30_000;

/// Attempts are clamped to this range: <2 defeats the point of a tournament,
/// >4 blows the judge's context budget (see spec non-goals).
const MIN_ATTEMPTS: i64 = 2;
const MAX_ATTEMPTS: i64 = 4;
const DEFAULT_ATTEMPTS: i64 = 3;

pub struct TournamentTool {
    provider: Arc<dyn Provider>,
    registry: Registry,
}

impl TournamentTool {
    pub fn new(provider: Arc<dyn Provider>, registry: Registry) -> Self {
        Self { provider, registry }
    }
}

#[derive(Deserialize)]
struct TournamentInput {
    prompt: String,
    #[serde(default)]
    attempts: Option<i64>,
    #[serde(default)]
    judge_criteria: Option<String>,
    #[serde(default)]
    model: Option<String>,
    #[serde(default)]
    cleanup: Option<bool>,
}

#[derive(Deserialize)]
struct JudgeScoreOutput {
    attempt: i64,
    score: f64,
    #[serde(default)]
    notes: String,
}

#[derive(Deserialize)]
struct JudgeOutput {
    winner: i64,
    reasoning: String,
    #[serde(default)]
    scores: Vec<JudgeScoreOutput>,
}

struct AttemptResult {
    attempt: usize,
    worktree: PathBuf,
    session_id: String,
    final_text: String,
    diff: String,
}

enum JudgeOutcome {
    Success(JudgeOutput),
    Failed(String),
}

#[async_trait]
impl Tool for TournamentTool {
    fn name(&self) -> &str {
        "tournament"
    }

    fn description(&self) -> &str {
        "Run best-of-N isolated attempts at a task in separate git worktrees and have a judge pick the winner."
    }

    fn parameters_schema(&self) -> Value {
        tournament_parameters_schema()
    }

    async fn execute(&self, input: Value, ctx: ToolContext) -> Result<ToolOutput> {
        let params: TournamentInput = serde_json::from_value(input)?;
        let working_dir = ctx
            .working_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));

        if run_git(&working_dir, &["rev-parse", "--git-dir"]).await.is_none() {
            return Err(anyhow::anyhow!(
                "tournament requires a git repository (working_dir is not inside a git repo)"
            ));
        }
        let repo_root = run_git(&working_dir, &["rev-parse", "--show-toplevel"])
            .await
            .ok_or_else(|| anyhow::anyhow!("failed to resolve git repo root"))?;
        let repo_root = PathBuf::from(repo_root);

        let attempts = clamp_attempts(params.attempts);
        let jcode_home = jcode_storage::jcode_dir()?;
        let parent_session_id = ctx.session_id.clone();

        let mut worktrees: Vec<PathBuf> = Vec::with_capacity(attempts);
        for i in 1..=attempts {
            let wt = worktree_path(&jcode_home, &parent_session_id, i);
            if let Some(parent) = wt.parent() {
                let _ = tokio::fs::create_dir_all(parent).await;
            }
            let output = tokio::process::Command::new("git")
                .arg("-C")
                .arg(&repo_root)
                .args(["worktree", "add", "--detach"])
                .arg(&wt)
                .arg("HEAD")
                .output()
                .await?;
            if !output.status.success() {
                for created in &worktrees {
                    remove_worktree(&repo_root, created).await;
                }
                return Err(anyhow::anyhow!(
                    "git worktree add failed for attempt {}: {}",
                    i,
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
            worktrees.push(wt);
        }

        let mut allowed: HashSet<String> = self.registry.tool_names().await.into_iter().collect();
        for blocked in ["subagent", "task", "todo", "todowrite", "todoread", "tournament"] {
            allowed.remove(blocked);
        }
        crate::config::config()
            .tools
            .apply_to_allowed_set(&mut allowed);

        let resolved_model = params
            .model
            .clone()
            .unwrap_or_else(|| self.provider.model());

        use futures::StreamExt;
        let mut stream: futures::stream::FuturesUnordered<_> = worktrees
            .iter()
            .enumerate()
            .map(|(idx, wt)| {
                let attempt = idx + 1;
                let wt = wt.clone();
                let provider = self.provider.fork();
                let registry = self.registry.clone();
                let allowed = allowed.clone();
                let parent_session_id = parent_session_id.clone();
                let prompt = params.prompt.clone();
                let model = resolved_model.clone();
                async move {
                    run_attempt(attempt, wt, provider, registry, allowed, parent_session_id, prompt, model)
                        .await
                }
            })
            .collect();

        let mut attempt_results: Vec<AttemptResult> = Vec::with_capacity(attempts);
        while let Some(result) = stream.next().await {
            attempt_results.push(result);
        }
        attempt_results.sort_by_key(|r| r.attempt);

        let judge_prompt = build_judge_prompt(
            &params.prompt,
            params.judge_criteria.as_deref(),
            &attempt_results,
        );
        let augmented_judge_prompt = format!(
            "{}\n\n## Output contract\nYour FINAL message must be exactly one JSON object (no prose, no code fences) conforming to this JSON Schema:\n{}",
            judge_prompt,
            serde_json::to_string_pretty(&judge_output_schema())
                .unwrap_or_else(|_| judge_output_schema().to_string())
        );

        let mut judge_session = Session::create(
            Some(parent_session_id.clone()),
            Some(format!("Tournament judge ({} attempts)", attempts)),
        );
        judge_session.model = Some(resolved_model.clone());
        if let Some(ref wd) = ctx.working_dir {
            judge_session.working_dir = Some(wd.display().to_string());
        }
        judge_session.save()?;
        let mut judge_agent = Agent::new_with_session(
            self.provider.fork(),
            self.registry.clone(),
            judge_session,
            Some(allowed.clone()),
        );

        let judge_outcome = match judge_agent.run_once_capture(&augmented_judge_prompt).await {
            Ok(text) => match parse_judge_output(&text) {
                Ok(judge) if (1..=attempts as i64).contains(&judge.winner) => {
                    JudgeOutcome::Success(judge)
                }
                Ok(judge) => JudgeOutcome::Failed(format!(
                    "judge returned out-of-range winner index {}",
                    judge.winner
                )),
                Err(err) => JudgeOutcome::Failed(err),
            },
            Err(err) => {
                logging::warn(&format!("[tool:tournament] judge agent failed: {}", err));
                JudgeOutcome::Failed(format!("judge agent execution failed: {}", err))
            }
        };

        let cleanup = params.cleanup.unwrap_or(true);
        let mut winner: Option<&AttemptResult> = None;
        if let JudgeOutcome::Success(ref judge) = judge_outcome {
            winner = attempt_results
                .iter()
                .find(|r| r.attempt as i64 == judge.winner);
            if cleanup && winner.is_some() {
                let winner_attempt = judge.winner as usize;
                for result in &attempt_results {
                    if result.attempt != winner_attempt {
                        remove_worktree(&repo_root, &result.worktree).await;
                    }
                }
            }
        }

        let output = format_output(&attempt_results, &judge_outcome, winner, cleanup);

        let attempt_sessions: Vec<Value> = attempt_results
            .iter()
            .map(|r| {
                json!({
                    "attempt": r.attempt,
                    "sessionId": r.session_id,
                    "worktree": r.worktree.display().to_string(),
                })
            })
            .collect();

        let metadata = json!({
            "attempts": attempt_results.len(),
            "winner": winner.map(|w| w.attempt),
            "worktreePath": winner.map(|w| w.worktree.display().to_string()),
            "attemptSessions": attempt_sessions,
            "judgeFailed": matches!(judge_outcome, JudgeOutcome::Failed(_)),
        });

        Ok(ToolOutput::new(output)
            .with_title(format!("Tournament ({} attempts)", attempts))
            .with_metadata(metadata))
    }
}

async fn run_attempt(
    attempt: usize,
    worktree: PathBuf,
    provider: Arc<dyn Provider>,
    registry: Registry,
    allowed: HashSet<String>,
    parent_session_id: String,
    prompt: String,
    model: String,
) -> AttemptResult {
    let mut session = Session::create(
        Some(parent_session_id),
        Some(format!("Tournament attempt {}", attempt)),
    );
    session.model = Some(model);
    session.working_dir = Some(worktree.display().to_string());
    let session_id = session.id.clone();
    if let Err(err) = session.save() {
        return AttemptResult {
            attempt,
            worktree: worktree.clone(),
            session_id,
            final_text: format!("[attempt {} failed to save session: {}]", attempt, err),
            diff: String::new(),
        };
    }

    let mut agent = Agent::new_with_session(provider, registry, session, Some(allowed));
    let final_text = match agent
        .run_once_capture(&attempt_prompt(&prompt, &worktree))
        .await
    {
        Ok(text) => text,
        Err(err) => {
            logging::warn(&format!(
                "[tool:tournament] attempt {} failed: {}",
                attempt, err
            ));
            format!("[attempt {} failed: {}]", attempt, err)
        }
    };
    let diff = capture_worktree_diff(&worktree).await;

    AttemptResult {
        attempt,
        worktree,
        session_id,
        final_text,
        diff,
    }
}

fn attempt_prompt(prompt: &str, worktree: &Path) -> String {
    format!(
        "{}\n\nYou are running in an isolated git worktree at `{}`. Work only inside this directory; do not touch files outside it. When finished, give your final answer summarizing what you changed.",
        prompt,
        worktree.display()
    )
}

async fn capture_worktree_diff(worktree: &Path) -> String {
    let _ = tokio::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .args(["add", "-A", "--intent-to-add"])
        .output()
        .await;
    match tokio::process::Command::new("git")
        .arg("-C")
        .arg(worktree)
        .arg("diff")
        .output()
        .await
    {
        Ok(out) => truncate_diff_tail(&String::from_utf8_lossy(&out.stdout), MAX_DIFF_CHARS),
        Err(_) => String::new(),
    }
}

async fn run_git(dir: &Path, args: &[&str]) -> Option<String> {
    let output = tokio::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

async fn remove_worktree(repo_root: &Path, worktree: &Path) {
    let result = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["worktree", "remove", "--force"])
        .arg(worktree)
        .output()
        .await;
    if let Err(err) = result {
        logging::warn(&format!(
            "[tool:tournament] failed to remove worktree {}: {}",
            worktree.display(),
            err
        ));
    }
}

fn build_judge_prompt(task: &str, criteria: Option<&str>, attempts: &[AttemptResult]) -> String {
    let mut prompt = String::new();
    prompt.push_str(
        "You are judging a tournament of independent attempts at the same coding task. Pick the single best attempt.\n\n",
    );
    prompt.push_str("## Task\n");
    prompt.push_str(task);
    prompt.push('\n');
    if let Some(criteria) = criteria {
        prompt.push_str("\n## Judge criteria\n");
        prompt.push_str(criteria);
        prompt.push('\n');
    }
    for result in attempts {
        prompt.push_str(&format!(
            "\n## Attempt {}\n### Final answer\n{}\n\n### Diff\n```diff\n{}\n```\n",
            result.attempt, result.final_text, result.diff
        ));
    }
    prompt
}

fn judge_output_schema() -> Value {
    json!({
        "type": "object",
        "required": ["winner", "reasoning", "scores"],
        "properties": {
            "winner": {
                "type": "integer",
                "description": "1-based index of the winning attempt."
            },
            "reasoning": {
                "type": "string"
            },
            "scores": {
                "type": "array",
                "items": {
                    "type": "object",
                    "required": ["attempt", "score", "notes"],
                    "properties": {
                        "attempt": { "type": "integer" },
                        "score": { "type": "number" },
                        "notes": { "type": "string" }
                    }
                }
            }
        }
    })
}

/// Fence-tolerant judge output parse, reusing SubagentTool's structural JSON
/// check (`enforce_structured_output`) before decoding into `JudgeOutput`.
/// Any failure here (garbage, missing fields, wrong types) is the signal to
/// fall back to keeping all worktrees.
fn parse_judge_output(text: &str) -> Result<JudgeOutput, String> {
    let canonical = super::task::enforce_structured_output(text)?;
    serde_json::from_str::<JudgeOutput>(&canonical)
        .map_err(|err| format!("judge output did not match schema: {}", err))
}

/// Tail-safe truncation: keeps the END of the diff (most recent hunks matter
/// most and this matches the truncate_tail pattern used for memory rerank).
fn truncate_diff_tail(s: &str, max: usize) -> String {
    let count = s.chars().count();
    if count <= max {
        return s.to_string();
    }
    let skipped = count - max;
    let tail: String = s.chars().skip(skipped).collect();
    format!(
        "...[diff truncated, showing last {} of {} chars]...\n{}",
        max, count, tail
    )
}

/// Clamp requested attempts into [2, 4], defaulting to 3 when unset.
fn clamp_attempts(requested: Option<i64>) -> usize {
    requested
        .unwrap_or(DEFAULT_ATTEMPTS)
        .clamp(MIN_ATTEMPTS, MAX_ATTEMPTS) as usize
}

fn worktree_path(home: &Path, parent_session: &str, attempt: usize) -> PathBuf {
    home.join("tournaments")
        .join(parent_session)
        .join(format!("attempt-{}", attempt))
}

fn format_output(
    attempts: &[AttemptResult],
    judge_outcome: &JudgeOutcome,
    winner: Option<&AttemptResult>,
    cleanup: bool,
) -> String {
    let mut output = String::new();
    let failure_reason = match judge_outcome {
        JudgeOutcome::Failed(reason) => Some(reason.clone()),
        JudgeOutcome::Success(_) if winner.is_none() => {
            Some("judge did not select a valid winner".to_string())
        }
        JudgeOutcome::Success(_) => None,
    };

    if let (JudgeOutcome::Success(judge), Some(winner)) = (judge_outcome, winner) {
        output.push_str(&format!(
            "# Tournament result: attempt {} wins\n\n## Judge reasoning\n{}\n\n## Scores\n",
            winner.attempt, judge.reasoning
        ));
        for score in &judge.scores {
            output.push_str(&format!(
                "- attempt {}: {} — {}\n",
                score.attempt, score.score, score.notes
            ));
        }
        output.push_str(&format!(
            "\n## Winning diff (attempt {})\n```diff\n{}\n```\n",
            winner.attempt, winner.diff
        ));
        output.push_str(&format!(
            "\nWinner worktree path: {} (apply with: git apply {}.diff or continue in the worktree)\n",
            winner.worktree.display(),
            winner.worktree.display()
        ));
        output.push_str(if cleanup {
            "\nLosing worktrees removed.\n"
        } else {
            "\nAll worktrees kept (cleanup=false).\n"
        });
    } else if let Some(reason) = failure_reason {
        output.push_str(&format!(
            "# Tournament judge failed: {}\n\nAll worktrees kept — judge failed, pick manually.\n",
            reason
        ));
        for result in attempts {
            output.push_str(&format!(
                "\n## Attempt {}\nWorktree: {}\nSession: {}\n### Final answer\n{}\n### Diff\n```diff\n{}\n```\n",
                result.attempt,
                result.worktree.display(),
                result.session_id,
                result.final_text,
                result.diff
            ));
        }
    }
    output
}

fn tournament_parameters_schema() -> Value {
    json!({
        "type": "object",
        "required": ["prompt"],
        "properties": {
            "intent": super::intent_schema_property(),
            "prompt": {
                "type": "string",
                "description": "Task prompt run in parallel across attempts."
            },
            "attempts": {
                "type": "integer",
                "description": "Number of parallel attempts. Clamped to 2..=4, default 3.",
                "minimum": 2,
                "maximum": 4,
                "default": 3
            },
            "judge_criteria": {
                "type": "string",
                "description": "Optional extra criteria folded into the judge prompt."
            },
            "model": {
                "type": "string",
                "description": "Model override for attempt and judge children."
            },
            "cleanup": {
                "type": "boolean",
                "description": "Remove losing worktrees after judging. Defaults to true. Winner's worktree is always kept.",
                "default": true
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_attempts_defaults_to_three() {
        assert_eq!(clamp_attempts(None), 3);
    }

    #[test]
    fn clamp_attempts_clamps_low_and_high() {
        assert_eq!(clamp_attempts(Some(0)), 2);
        assert_eq!(clamp_attempts(Some(1)), 2);
        assert_eq!(clamp_attempts(Some(10)), 4);
    }

    #[test]
    fn clamp_attempts_passes_through_in_range() {
        assert_eq!(clamp_attempts(Some(2)), 2);
        assert_eq!(clamp_attempts(Some(3)), 3);
        assert_eq!(clamp_attempts(Some(4)), 4);
    }

    #[test]
    fn worktree_path_builds_expected_layout() {
        let home = Path::new("/home/user/.jcode");
        let path = worktree_path(home, "parent-session-1", 2);
        assert_eq!(
            path,
            PathBuf::from("/home/user/.jcode/tournaments/parent-session-1/attempt-2")
        );
    }

    #[test]
    fn truncate_diff_tail_leaves_short_diff_untouched() {
        let diff = "short diff";
        assert_eq!(truncate_diff_tail(diff, 30_000), diff);
    }

    #[test]
    fn truncate_diff_tail_boundary_exact_length_untouched() {
        let diff = "a".repeat(100);
        assert_eq!(truncate_diff_tail(&diff, 100), diff);
    }

    #[test]
    fn truncate_diff_tail_truncates_and_keeps_tail() {
        let diff = format!("{}{}", "x".repeat(50), "y".repeat(50));
        let truncated = truncate_diff_tail(&diff, 50);
        assert!(truncated.contains("truncated"));
        assert!(truncated.ends_with(&"y".repeat(50)));
        assert!(!truncated.contains('x'));
    }

    #[test]
    fn parse_judge_output_accepts_valid_json() {
        let text = r#"{"winner": 2, "reasoning": "attempt 2 is cleaner", "scores": [{"attempt": 1, "score": 5.0, "notes": "ok"}, {"attempt": 2, "score": 8.0, "notes": "better"}]}"#;
        let judge = parse_judge_output(text).expect("should parse");
        assert_eq!(judge.winner, 2);
        assert_eq!(judge.reasoning, "attempt 2 is cleaner");
        assert_eq!(judge.scores.len(), 2);
    }

    #[test]
    fn parse_judge_output_accepts_fenced_json() {
        let text = "```json\n{\"winner\": 1, \"reasoning\": \"ok\", \"scores\": []}\n```";
        let judge = parse_judge_output(text).expect("should parse");
        assert_eq!(judge.winner, 1);
    }

    #[test]
    fn parse_judge_output_rejects_garbage_as_keep_all_signal() {
        let text = "the winner is clearly attempt 2, trust me";
        let result = parse_judge_output(text);
        assert!(result.is_err());
    }

    #[test]
    fn parse_judge_output_rejects_json_missing_required_fields() {
        let text = r#"{"reasoning": "no winner field"}"#;
        let result = parse_judge_output(text);
        assert!(result.is_err());
    }

    #[test]
    fn tournament_parameters_schema_requires_prompt() {
        let schema = tournament_parameters_schema();
        assert_eq!(schema["required"][0], "prompt");
        assert!(schema["properties"]["attempts"]["minimum"] == 2);
        assert!(schema["properties"]["attempts"]["maximum"] == 4);
    }
}
