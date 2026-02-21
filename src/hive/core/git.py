"""Git operations.

Minimal git helpers for Hive — read-only queries and basic operations.
Worktree creation/removal is handled by Swarm.
"""

import subprocess
from pathlib import Path
from typing import Any

from hive.core.exceptions import GitError


def run_git(
    args: list[str],
    cwd: Path | None = None,
    check: bool = True,
    capture_output: bool = True,
) -> subprocess.CompletedProcess[str]:
    """Run a git command safely."""
    cmd = ["git"] + args
    try:
        result = subprocess.run(
            cmd,
            cwd=cwd,
            capture_output=capture_output,
            text=True,
            timeout=300,
        )
        if check and result.returncode != 0:
            error_msg = result.stderr.strip() or result.stdout.strip()
            raise GitError(f"git {' '.join(args)} failed: {error_msg}")
        return result
    except subprocess.TimeoutExpired as e:
        raise GitError(f"git {' '.join(args)} timed out") from e
    except FileNotFoundError as e:
        raise GitError("git not found in PATH") from e


def is_git_repo(path: Path) -> bool:
    """Check if path is a git repository."""
    if not path.exists():
        return False
    try:
        result = run_git(
            ["rev-parse", "--git-dir"],
            cwd=path,
            check=False,
        )
        return result.returncode == 0
    except GitError:
        return False


def fetch_origin(repo_path: Path, prune: bool = True) -> None:
    """Fetch from origin."""
    args = ["fetch", "origin"]
    if prune:
        args.append("--prune")
    run_git(args, cwd=repo_path)


def get_current_branch(repo_path: Path) -> str | None:
    """Get the current branch name."""
    try:
        result = run_git(
            ["rev-parse", "--abbrev-ref", "HEAD"],
            cwd=repo_path,
        )
        branch = result.stdout.strip()
        return None if branch == "HEAD" else branch
    except GitError:
        return None


def is_dirty(repo_path: Path) -> bool:
    """Check if working tree has uncommitted changes."""
    try:
        result = run_git(
            ["status", "--porcelain"],
            cwd=repo_path,
        )
        return bool(result.stdout.strip())
    except GitError:
        return False


def get_ahead_behind(repo_path: Path, branch: str | None = None) -> tuple[int, int]:
    """Get commits ahead/behind upstream."""
    if branch is None:
        branch = get_current_branch(repo_path)
    if not branch:
        return (0, 0)

    try:
        result = run_git(
            ["rev-list", "--left-right", "--count", f"{branch}...origin/{branch}"],
            cwd=repo_path,
            check=False,
        )
        if result.returncode != 0:
            return (0, 0)
        parts = result.stdout.strip().split()
        if len(parts) == 2:
            return (int(parts[0]), int(parts[1]))
        return (0, 0)
    except (GitError, ValueError):
        return (0, 0)


def get_last_commit(repo_path: Path) -> dict[str, str] | None:
    """Get info about the last commit."""
    try:
        result = run_git(
            ["log", "-1", "--format=%H%n%s"],
            cwd=repo_path,
        )
        lines = result.stdout.strip().split("\n")
        if len(lines) >= 2:
            return {
                "sha": lines[0][:12],
                "subject": lines[1],
            }
        return None
    except GitError:
        return None


def get_git_status(repo_path: Path) -> dict[str, Any]:
    """Get git status information."""
    branch = get_current_branch(repo_path)
    ahead, behind = get_ahead_behind(repo_path, branch)
    last_commit = get_last_commit(repo_path)

    return {
        "branch": branch,
        "dirty": is_dirty(repo_path),
        "ahead": ahead,
        "behind": behind,
        "last_commit": last_commit,
    }


def clone_repo(url: str, dest: Path) -> None:
    """Clone a repository."""
    run_git(["clone", url, str(dest)])
