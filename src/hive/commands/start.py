"""hive start - Start work on a quest or umbrella issue.

Delegates worktree creation and agent launching to Swarm.
"""

import json
import subprocess
from pathlib import Path
from typing import Annotated, Any

import typer

from hive.core.context import get_context
from hive.core.exceptions import WorkspaceError
from hive.core.hive_state import load_state


def start(
    ctx: typer.Context,
    quest_id: Annotated[
        str,
        typer.Argument(help="Quest or umbrella issue number."),
    ],
    repos: Annotated[
        str | None,
        typer.Option(
            "--repos", "-r",
            help="Comma-separated list of repos to work on (default: all).",
        ),
    ] = None,
    no_claude: Annotated[
        bool,
        typer.Option("--no-claude", help="Don't start agents (worktrees only)."),
    ] = False,
    json_output: Annotated[
        bool,
        typer.Option("--json", help="Output as JSON."),
    ] = False,
) -> None:
    """Start work on a quest.

    Resolves quest details, then delegates to Swarm for worktree creation
    and agent launching.

    \b
    What this does:
    1. Loads quest from state.json (or issues.json for legacy tasks)
    2. Calls `swarm create` for each repo to set up worktrees + agents
    3. Writes TASK.md and CLAUDE.local.md in each worktree
    """
    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)

    # Load workspace
    if not context.has_workspace():
        _error("workspace.yaml not found", json_output, exit_code=3)

    try:
        workspace = context.load_workspace()
    except WorkspaceError as e:
        _error(str(e), json_output, exit_code=3)

    # Resolve task source: quest state OR legacy issues.json
    task_info = _resolve_task(context, quest_id, repos, json_output)
    selected_repos = task_info["selected_repos"]
    umbrella = task_info["umbrella"]
    repo_issues = task_info["repo_issues"]

    if not json_output:
        typer.echo(f"Starting work on #{quest_id}")
        typer.echo(f"  Repos: {', '.join(selected_repos)}")
        typer.echo()

    # Ensure directories exist
    context.ensure_dirs()

    # Process each repo via Swarm
    results: list[dict[str, Any]] = []

    for repo_key in selected_repos:
        result = _start_repo(
            context=context,
            workspace=workspace,
            quest_id=quest_id,
            umbrella=umbrella,
            repo_key=repo_key,
            repo_issue=repo_issues.get(repo_key, {}),
            json_output=json_output,
            with_agent=not no_claude,
        )
        results.append(result)

    # Output results
    if json_output:
        output = {
            "quest_id": quest_id,
            "umbrella_url": umbrella.get("url"),
            "repos": {r["repo_key"]: r for r in results},
        }
        typer.echo(json.dumps(output, indent=2))
    else:
        typer.echo()
        _print_summary(results)
        typer.echo()
        typer.echo("Worktrees ready. Run `hive up` to attach to the session.")


def _resolve_task(
    context,
    quest_id: str,
    repos_filter: str | None,
    json_output: bool,
) -> dict[str, Any]:
    """Resolve task from quest state or legacy issues.json.

    Returns dict with: selected_repos, umbrella, repo_issues.
    """
    # Try legacy issues.json first
    task_dir = context.task_dir(quest_id)
    issues_path = task_dir / "issues.json"

    if issues_path.exists():
        return _resolve_from_issues_json(
            issues_path, repos_filter, json_output,
        )

    # Try quest from state.json
    state = load_state(context.hive_dir)
    quest = state.get_quest(quest_id)

    if quest is not None:
        return _resolve_from_quest(quest, repos_filter, json_output)

    _error(
        f"Quest #{quest_id} not found. "
        "Run 'hive plan' or 'hive issue new' first.",
        json_output,
        exit_code=1,
    )
    return {}  # unreachable, _error raises


def _resolve_from_issues_json(
    issues_path: Path,
    repos_filter: str | None,
    json_output: bool,
) -> dict[str, Any]:
    """Resolve task from legacy issues.json."""
    try:
        with open(issues_path) as f:
            issues_data = json.load(f)
    except (json.JSONDecodeError, OSError) as e:
        _error(f"Failed to load issues.json: {e}", json_output, exit_code=1)

    umbrella = issues_data.get("umbrella", {})
    repo_issues = issues_data.get("repos", {})

    if not repo_issues:
        _error("No repo issues found in issues.json", json_output, exit_code=1)

    if repos_filter:
        selected = [r.strip() for r in repos_filter.split(",") if r.strip()]
        for repo_key in selected:
            if repo_key not in repo_issues:
                _error(
                    f"Repo '{repo_key}' not found in issues.json",
                    json_output,
                    exit_code=1,
                )
    else:
        selected = list(repo_issues.keys())

    return {
        "selected_repos": selected,
        "umbrella": umbrella,
        "repo_issues": repo_issues,
    }


def _resolve_from_quest(
    quest, repos_filter: str | None, json_output: bool,
) -> dict[str, Any]:
    """Resolve task from quest state."""
    if repos_filter:
        selected = [r.strip() for r in repos_filter.split(",") if r.strip()]
    elif quest.repos:
        selected = list(quest.repos)
    else:
        _error(
            f"Quest #{quest.id} has no repos. "
            "Use --repos to specify which repos to work on.",
            json_output,
            exit_code=1,
        )

    umbrella: dict[str, Any] = {
        "title": quest.title,
        "goal": quest.goal,
    }

    repo_issues: dict[str, dict[str, Any]] = {
        repo_key: {} for repo_key in selected
    }

    return {
        "selected_repos": selected,
        "umbrella": umbrella,
        "repo_issues": repo_issues,
    }


def _start_repo(
    context,
    workspace,
    quest_id: str,
    umbrella: dict[str, Any],
    repo_key: str,
    repo_issue: dict[str, Any],
    json_output: bool,
    with_agent: bool = True,
) -> dict[str, Any]:
    """Start work on a single repo via Swarm.

    Calls `swarm create` to set up worktree + agent pane.
    """
    result: dict[str, Any] = {
        "repo_key": repo_key,
        "status": "unknown",
    }

    # Get repo path from workspace
    repo_config = workspace.get_repo(repo_key)
    if repo_config is None:
        result["status"] = "error"
        result["error"] = "repo not in workspace.yaml"
        if not json_output:
            typer.secho(f"  {repo_key}: error (not in workspace.yaml)", fg="red")
        return result

    repo_path = repo_config.path
    if not repo_path.exists():
        result["status"] = "error"
        result["error"] = f"repo path does not exist: {repo_path}"
        if not json_output:
            typer.secho(f"  {repo_key}: error (repo path missing)", fg="red")
        return result

    # Build task description for the agent
    task_desc = _build_task_description(quest_id, umbrella, repo_key, repo_issue)

    if not json_output:
        typer.echo(f"  {repo_key}: creating via swarm...")

    # Delegate to swarm for worktree + agent creation
    try:
        swarm_result = _call_swarm_create(
            repo_path=repo_path,
            prompt=task_desc,
            with_agent=with_agent,
        )
        result["status"] = "created"
        result["worktree_path"] = swarm_result.get("worktree_path", "")
        result["branch"] = swarm_result.get("branch", "")

        if not json_output:
            typer.secho(f"  {repo_key}: created", fg="green")

    except SwarmError as e:
        result["status"] = "error"
        result["error"] = str(e)
        if not json_output:
            typer.secho(f"  {repo_key}: swarm error - {e}", fg="red")

    return result


def _build_task_description(
    quest_id: str,
    umbrella: dict[str, Any],
    repo_key: str,
    repo_issue: dict[str, Any],
) -> str:
    """Build a task description string for the swarm agent."""
    parts = [f"Quest #{quest_id}"]

    title = umbrella.get("title")
    if title:
        parts.append(f": {title}")

    goal = umbrella.get("goal")
    if goal and goal != title:
        parts.append(f"\nGoal: {goal}")

    parts.append(f"\nRepo: {repo_key}")

    repo_url = repo_issue.get("url")
    if repo_url:
        parts.append(f"\nIssue: {repo_url}")

    return "".join(parts)


class SwarmError(Exception):
    """Error from swarm CLI."""


def _call_swarm_create(
    repo_path: Path,
    prompt: str,
    with_agent: bool = True,
) -> dict[str, Any]:
    """Call `swarm create` to set up a worktree + agent.

    Returns dict with worktree_path, branch, etc.
    """
    cmd = ["swarm", "create", prompt]
    if not with_agent:
        cmd.append("--no-agent")

    try:
        proc = subprocess.run(
            cmd,
            cwd=repo_path,
            capture_output=True,
            text=True,
            timeout=60,
        )
    except FileNotFoundError as e:
        raise SwarmError(
            "swarm not found in PATH. Install swarm: cargo install --path ../swarm"
        ) from e
    except subprocess.TimeoutExpired as e:
        raise SwarmError("swarm create timed out") from e

    if proc.returncode != 0:
        error_msg = proc.stderr.strip() or proc.stdout.strip()
        raise SwarmError(f"swarm create failed: {error_msg}")

    # Parse swarm JSON output
    try:
        return json.loads(proc.stdout)
    except json.JSONDecodeError:
        # swarm may not return JSON — return what we can
        return {"output": proc.stdout.strip()}


def _print_summary(results: list[dict[str, Any]]) -> None:
    """Print summary of worktree setup."""
    created = sum(1 for r in results if r["status"] == "created")
    exists = sum(1 for r in results if r["status"] == "exists")
    errors = sum(1 for r in results if r["status"] == "error")

    typer.echo("Summary:")
    if created:
        typer.secho(f"  Created: {created}", fg="green")
    if exists:
        typer.echo(f"  Already existed: {exists}")
    if errors:
        typer.secho(f"  Errors: {errors}", fg="red")

    worktrees = [r for r in results if r.get("worktree_path")]
    if worktrees:
        typer.echo()
        typer.echo("Worktrees:")
        for r in worktrees:
            typer.echo(f"  {r['repo_key']}: {r['worktree_path']}")


def _error(message: str, json_output: bool, exit_code: int = 1) -> None:
    """Output error and exit."""
    if json_output:
        typer.echo(json.dumps({"error": message}))
    else:
        typer.secho(f"Error: {message}", fg="red", err=True)
    raise typer.Exit(exit_code)
