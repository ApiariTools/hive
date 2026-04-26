use serde::Serialize;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tracing::{info, warn};

pub type PrReviewCache = Arc<Mutex<HashMap<i64, PrReviewStatus>>>;

#[derive(Clone, Serialize, Default, Debug)]
pub struct PrReviewStatus {
    pub review_state: Option<String>,
    pub ci_status: Option<String>,
    pub total_comments: u32,
    pub open_comments: u32,
    pub resolved_comments: u32,
}

struct PrInfo {
    owner: String,
    repo: String,
    number: i64,
}

/// Parse a GitHub PR URL into owner/repo/number.
fn parse_pr_url(url: &str) -> Option<PrInfo> {
    // https://github.com/OWNER/REPO/pull/NUMBER
    let url = url.trim_end_matches('/');
    let parts: Vec<&str> = url.split('/').collect();
    // [..., "github.com", OWNER, REPO, "pull", NUMBER]
    if parts.len() < 5 {
        return None;
    }
    let len = parts.len();
    if parts[len - 2] != "pull" {
        return None;
    }
    let number = parts[len - 1].parse::<i64>().ok()?;
    let repo = parts[len - 3].to_string();
    let owner = parts[len - 4].to_string();
    Some(PrInfo {
        owner,
        repo,
        number,
    })
}

/// Build the GraphQL fragment for a single PR.
fn pr_fragment() -> &'static str {
    r#"{
      reviewDecision
      reviews(last: 10) { nodes { state } }
      reviewThreads(first: 100) {
        nodes { isResolved }
      }
      commits(last: 1) {
        nodes {
          commit {
            statusCheckRollup { state }
          }
        }
      }
    }"#
}

/// Build a GraphQL query for multiple PRs in the same repo using aliases.
fn build_query(owner: &str, repo: &str, numbers: &[i64]) -> String {
    let mut fields = String::new();
    for (i, num) in numbers.iter().enumerate() {
        fields.push_str(&format!(
            "    pr{i}: pullRequest(number: {num}) {}\n",
            pr_fragment()
        ));
    }
    format!("query {{\n  repository(owner: \"{owner}\", name: \"{repo}\") {{\n{fields}  }}\n}}")
}

/// Parse the review status from a single PR's GraphQL response.
fn parse_pr_response(pr_data: &serde_json::Value) -> PrReviewStatus {
    let review_state = pr_data
        .get("reviewDecision")
        .and_then(|v| v.as_str())
        .map(|s| match s {
            "APPROVED" => "APPROVED".to_string(),
            "CHANGES_REQUESTED" => "CHANGES_REQUESTED".to_string(),
            "REVIEW_REQUIRED" => "PENDING".to_string(),
            other => other.to_string(),
        });

    // Count review threads
    let mut open_comments: u32 = 0;
    let mut resolved_comments: u32 = 0;
    if let Some(threads) = pr_data
        .get("reviewThreads")
        .and_then(|t| t.get("nodes"))
        .and_then(|n| n.as_array())
    {
        for thread in threads {
            if thread
                .get("isResolved")
                .and_then(|r| r.as_bool())
                .unwrap_or(false)
            {
                resolved_comments += 1;
            } else {
                open_comments += 1;
            }
        }
    }

    // CI status from last commit
    let ci_status = pr_data
        .get("commits")
        .and_then(|c| c.get("nodes"))
        .and_then(|n| n.as_array())
        .and_then(|a| a.last())
        .and_then(|node| node.get("commit"))
        .and_then(|c| c.get("statusCheckRollup"))
        .and_then(|r| r.get("state"))
        .and_then(|s| s.as_str())
        .map(|s| s.to_string());

    PrReviewStatus {
        review_state,
        ci_status,
        total_comments: open_comments + resolved_comments,
        open_comments,
        resolved_comments,
    }
}

/// Read open PRs from .swarm/state.json, returning (pr_number, pr_url) pairs.
fn read_open_prs(workspace_root: &std::path::Path) -> Vec<(i64, String)> {
    let state_path = workspace_root.join(".swarm/state.json");
    let content = match std::fs::read_to_string(&state_path) {
        Ok(c) => c,
        Err(_) => return vec![],
    };
    let state: serde_json::Value = match serde_json::from_str(&content) {
        Ok(v) => v,
        Err(_) => return vec![],
    };

    let worktrees = match state.get("worktrees").and_then(|w| w.as_array()) {
        Some(w) => w,
        None => return vec![],
    };

    let mut prs = Vec::new();
    for wt in worktrees {
        let pr = match wt.get("pr") {
            Some(p) => p,
            None => continue,
        };

        // Only include open PRs (or PRs without explicit state, which we assume open)
        let pr_state = pr.get("state").and_then(|s| s.as_str()).unwrap_or("open");
        if pr_state == "closed" || pr_state == "merged" {
            continue;
        }

        if let Some(url) = pr.get("url").and_then(|u| u.as_str())
            && let Some(info) = parse_pr_url(url)
        {
            prs.push((info.number, url.to_string()));
        }
    }

    prs
}

/// Poll GitHub for PR review data and update the cache.
async fn poll_once(cache: &PrReviewCache, workspace_root: &std::path::Path) {
    let prs = read_open_prs(workspace_root);
    if prs.is_empty() {
        return;
    }

    // Group PRs by (owner, repo)
    let mut by_repo: HashMap<(String, String), Vec<i64>> = HashMap::new();
    for (number, url) in &prs {
        if let Some(info) = parse_pr_url(url) {
            by_repo
                .entry((info.owner, info.repo))
                .or_default()
                .push(*number);
        }
    }

    let mut new_data: HashMap<i64, PrReviewStatus> = HashMap::new();

    for ((owner, repo), numbers) in &by_repo {
        let query = build_query(owner, repo, numbers);

        let result = tokio::process::Command::new("gh")
            .args(["api", "graphql", "-f", &format!("query={query}")])
            .env_remove("GH_TOKEN")
            .output()
            .await;

        let output = match result {
            Ok(o) if o.status.success() => o,
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                warn!("gh graphql failed for {owner}/{repo}: {stderr}");
                continue;
            }
            Err(e) => {
                warn!("failed to run gh: {e}");
                continue;
            }
        };

        let body: serde_json::Value = match serde_json::from_slice(&output.stdout) {
            Ok(v) => v,
            Err(e) => {
                warn!("failed to parse gh response: {e}");
                continue;
            }
        };

        let repo_data = match body.get("data").and_then(|d| d.get("repository")) {
            Some(r) => r,
            None => {
                warn!("no repository data in gh response for {owner}/{repo}");
                continue;
            }
        };

        for (i, number) in numbers.iter().enumerate() {
            let alias = format!("pr{i}");
            if let Some(pr_data) = repo_data.get(&alias) {
                new_data.insert(*number, parse_pr_response(pr_data));
            }
        }
    }

    if !new_data.is_empty() {
        info!("updated PR review data for {} PRs", new_data.len());
        if let Ok(mut guard) = cache.lock() {
            *guard = new_data;
        }
    }
}

/// Start the background PR review poller. Runs immediately, then every 60s.
pub fn start_pr_review_poller(cache: PrReviewCache, workspace_root: PathBuf) {
    tokio::spawn(async move {
        loop {
            poll_once(&cache, &workspace_root).await;
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_pr_url_valid() {
        let info = parse_pr_url("https://github.com/ApiariTools/hive/pull/6").unwrap();
        assert_eq!(info.owner, "ApiariTools");
        assert_eq!(info.repo, "hive");
        assert_eq!(info.number, 6);
    }

    #[test]
    fn test_parse_pr_url_trailing_slash() {
        let info = parse_pr_url("https://github.com/Org/Repo/pull/42/").unwrap();
        assert_eq!(info.owner, "Org");
        assert_eq!(info.repo, "Repo");
        assert_eq!(info.number, 42);
    }

    #[test]
    fn test_parse_pr_url_invalid() {
        assert!(parse_pr_url("https://github.com/foo").is_none());
        assert!(parse_pr_url("not a url").is_none());
        assert!(parse_pr_url("https://github.com/o/r/issues/1").is_none());
    }

    #[test]
    fn test_build_query_single_pr() {
        let q = build_query("Org", "Repo", &[5]);
        assert!(q.contains("repository(owner: \"Org\", name: \"Repo\")"));
        assert!(q.contains("pr0: pullRequest(number: 5)"));
    }

    #[test]
    fn test_build_query_multiple_prs() {
        let q = build_query("Org", "Repo", &[1, 2, 3]);
        assert!(q.contains("pr0: pullRequest(number: 1)"));
        assert!(q.contains("pr1: pullRequest(number: 2)"));
        assert!(q.contains("pr2: pullRequest(number: 3)"));
    }

    #[test]
    fn test_parse_pr_response_approved() {
        let data = serde_json::json!({
            "reviewDecision": "APPROVED",
            "reviews": { "nodes": [{ "state": "APPROVED" }] },
            "reviewThreads": {
                "nodes": [
                    { "isResolved": true },
                    { "isResolved": false },
                    { "isResolved": true },
                ]
            },
            "commits": {
                "nodes": [{
                    "commit": {
                        "statusCheckRollup": { "state": "SUCCESS" }
                    }
                }]
            }
        });

        let status = parse_pr_response(&data);
        assert_eq!(status.review_state.as_deref(), Some("APPROVED"));
        assert_eq!(status.ci_status.as_deref(), Some("SUCCESS"));
        assert_eq!(status.total_comments, 3);
        assert_eq!(status.open_comments, 1);
        assert_eq!(status.resolved_comments, 2);
    }

    #[test]
    fn test_parse_pr_response_review_required() {
        let data = serde_json::json!({
            "reviewDecision": "REVIEW_REQUIRED",
            "reviews": { "nodes": [] },
            "reviewThreads": { "nodes": [] },
            "commits": {
                "nodes": [{
                    "commit": {
                        "statusCheckRollup": { "state": "PENDING" }
                    }
                }]
            }
        });

        let status = parse_pr_response(&data);
        assert_eq!(status.review_state.as_deref(), Some("PENDING"));
        assert_eq!(status.ci_status.as_deref(), Some("PENDING"));
        assert_eq!(status.total_comments, 0);
    }

    #[test]
    fn test_parse_pr_response_null_fields() {
        let data = serde_json::json!({
            "reviewDecision": null,
            "reviews": { "nodes": [] },
            "reviewThreads": { "nodes": [] },
            "commits": { "nodes": [] }
        });

        let status = parse_pr_response(&data);
        assert!(status.review_state.is_none());
        assert!(status.ci_status.is_none());
        assert_eq!(status.total_comments, 0);
    }

    #[test]
    fn test_read_open_prs_no_state() {
        let dir = tempfile::tempdir().unwrap();
        let prs = read_open_prs(dir.path());
        assert!(prs.is_empty());
    }

    #[test]
    fn test_read_open_prs_filters_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".swarm")).unwrap();
        std::fs::write(
            dir.path().join(".swarm/state.json"),
            r#"{"worktrees":[
                {"id":"w1","pr":{"url":"https://github.com/O/R/pull/1","state":"open"}},
                {"id":"w2","pr":{"url":"https://github.com/O/R/pull/2","state":"closed"}},
                {"id":"w3","pr":{"url":"https://github.com/O/R/pull/3","state":"merged"}},
                {"id":"w4","pr":{"url":"https://github.com/O/R/pull/4"}}
            ]}"#,
        )
        .unwrap();

        let prs = read_open_prs(dir.path());
        assert_eq!(prs.len(), 2);
        assert_eq!(prs[0].0, 1);
        assert_eq!(prs[1].0, 4); // no state = assumed open
    }
}
