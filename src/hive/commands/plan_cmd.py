"""hive plan - Create a new Quest."""

from typing import Annotated

import typer

from hive.core.context import get_context
from hive.core.hive_state import append_event, load_state, save_quest_local, save_state
from hive.core.quest_github import build_quest_body, create_quest_issue
from hive.models.quest import Quest


def plan(
    ctx: typer.Context,
    title: Annotated[
        str,
        typer.Argument(help="Quest title, e.g. 'Launch improved onboarding'."),
    ],
    repos: Annotated[
        str,
        typer.Option("--repos", help="Comma-separated repo names."),
    ] = "",
    goal: Annotated[
        str,
        typer.Option("--goal", help="Quest goal description."),
    ] = "",
) -> None:
    """Create a GitHub Quest issue and seed local quest folder."""
    root = ctx.obj.get("root") if ctx.obj else None
    context = get_context(root=root)
    hive_dir = context.hive_dir

    repo_list = [r.strip() for r in repos.split(",") if r.strip()] if repos else []
    quest_goal = goal or title

    # Determine issues repo from workspace config
    issues_repo = None
    if context.has_workspace():
        try:
            ws = context.load_workspace()
            if ws.provider and ws.provider.repo:
                issues_repo = ws.provider.repo
            elif ws.provider.type == "github" and ws.defaults.issues_repo:
                # Fallback: resolve from defaults.issues_repo key
                from hive.commands.issue_v2 import _get_repo_github_path

                repo_cfg = ws.get_repo(ws.defaults.issues_repo)
                if repo_cfg:
                    issues_repo = _get_repo_github_path(repo_cfg.path)
        except Exception:
            pass

    if not issues_repo:
        typer.secho(
            "Error: No issues repo configured. Set provider.repo (owner/repo) "
            "or defaults.issues_repo (repo key) in workspace.yaml.",
            fg="red",
            err=True,
        )
        raise typer.Exit(1)

    # Create GitHub issue
    body = build_quest_body(quest_goal, repo_list)
    try:
        result = create_quest_issue(issues_repo, title, body)
    except Exception as e:
        typer.secho(f"Error creating Quest issue: {e}", fg="red", err=True)
        raise typer.Exit(1)

    issue_number = str(result.get("number", ""))
    issue_url = result.get("url", "")

    # Create local Quest
    quest = Quest(
        id=issue_number,
        title=title,
        goal=quest_goal,
        repos=repo_list,
        updated_at=Quest.now_iso(),
    )

    # Update state
    state = load_state(hive_dir)
    state.add_quest(quest)
    save_state(hive_dir, state)
    save_quest_local(hive_dir, quest)

    append_event(hive_dir, {
        "type": "quest_created",
        "quest_id": issue_number,
        "title": title,
        "url": issue_url,
    })

    typer.echo(f"Quest #{issue_number} created: {title}")
    typer.echo(f"  {issue_url}")
