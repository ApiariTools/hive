"""GitHub-backed Quest operations."""

from hive.core.gh import create_issue, get_issue_info, update_issue_body

HIVE_STATUS_START = "<!-- HIVE_STATUS_START -->"
HIVE_STATUS_END = "<!-- HIVE_STATUS_END -->"


def create_quest_issue(repo: str, title: str, body: str) -> dict:
    """Create a GitHub issue with Quest prefix and label.

    Args:
        repo: Repository in owner/name format.
        title: Quest title (will be prefixed with [Quest]).
        body: Issue body markdown.

    Returns:
        Issue info dict with number, url, repo.
    """
    quest_title = f"[Quest] {title}" if not title.startswith("[Quest]") else title
    return create_issue(repo, quest_title, body)


def update_hive_status_block(body: str, new_status: str) -> str:
    """Replace content between HIVE_STATUS markers.

    If markers are missing, appends a new status section.

    Args:
        body: Full issue body.
        new_status: New status text to place between markers.

    Returns:
        Updated body with new status block.
    """
    start_idx = body.find(HIVE_STATUS_START)
    end_idx = body.find(HIVE_STATUS_END)

    if start_idx == -1 or end_idx == -1:
        # Markers missing - append status section
        section = (
            f"\n\n## Status (Managed by Hive)\n"
            f"{HIVE_STATUS_START}\n"
            f"{new_status}\n"
            f"{HIVE_STATUS_END}"
        )
        return body.rstrip() + section

    # Replace content between markers
    before = body[: start_idx + len(HIVE_STATUS_START)]
    after = body[end_idx:]
    return f"{before}\n{new_status}\n{after}"


def sync_quest_status(repo: str, issue_number: int, status_text: str) -> None:
    """Fetch issue, update HIVE_STATUS block, push back.

    Args:
        repo: Repository in owner/name format.
        issue_number: Issue number.
        status_text: New status text.
    """
    info = get_issue_info(repo, issue_number)
    if info is None:
        return

    current_body = info.get("body", "")
    updated_body = update_hive_status_block(current_body, status_text)

    if updated_body != current_body:
        update_issue_body(repo, issue_number, updated_body)


def build_quest_body(goal: str, repos: list[str]) -> str:
    """Build a Quest issue body with standard sections.

    Args:
        goal: Quest goal description.
        repos: List of repo names.

    Returns:
        Formatted markdown body.
    """
    repos_list = "\n".join(f"- {r}" for r in repos) if repos else "- (none yet)"
    return (
        f"## Goal\n{goal}\n\n"
        f"## Definition of Done\n- [ ] (to be defined)\n\n"
        f"## Repos\n{repos_list}\n\n"
        f"## Status (Managed by Hive)\n"
        f"{HIVE_STATUS_START}\n"
        f"Not started\n"
        f"{HIVE_STATUS_END}"
    )
