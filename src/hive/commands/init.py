"""hive init - Initialize workspace from GitHub org."""

import json
import re
import subprocess
from pathlib import Path
from typing import Annotated, Any, Optional

import typer

from hive.core.context import get_context
from hive.core.exceptions import GhError, GitError
from hive.core.gh import check_gh_auth, list_org_repos
from hive.core.git import clone_repo, fetch_origin, is_git_repo
from hive.core.workspace import create_workspace_yaml, update_workspace_repos


def init(
    ctx: typer.Context,
    org: Annotated[
        Optional[str],
        typer.Option("--org", help="GitHub organization to clone repos from."),
    ] = None,
    name: Annotated[
        Optional[str],
        typer.Option("--name", help="Project name for CLAUDE.md."),
    ] = None,
    description: Annotated[
        Optional[str],
        typer.Option("--description", help="Project description for CLAUDE.md."),
    ] = None,
    dest: Annotated[
        Optional[str],
        typer.Option("--dest", help="Destination directory for cloned repos."),
    ] = None,
    match: Annotated[
        Optional[str],
        typer.Option("--match", help="Regex pattern to match repo names."),
    ] = None,
    exclude: Annotated[
        Optional[str],
        typer.Option("--exclude", help="Regex pattern to exclude repo names."),
    ] = None,
    visibility: Annotated[
        str,
        typer.Option(
            "--visibility",
            help="Filter by visibility: all, public, private, internal.",
        ),
    ] = "all",
    fetch: Annotated[
        bool,
        typer.Option("--fetch", help="Fetch existing repos instead of skipping."),
    ] = False,
    ssh: Annotated[
        bool,
        typer.Option("--ssh", help="Clone using SSH URLs."),
    ] = True,
    https: Annotated[
        bool,
        typer.Option("--https", help="Clone using HTTPS URLs."),
    ] = False,
    json_output: Annotated[
        bool,
        typer.Option("--json", help="Output as JSON."),
    ] = False,
) -> None:
    """Initialize a hive workspace.

    With --org: clones repos from a GitHub organization.
    Without --org: interactively detects local repos and sets up workspace.

    If run from inside a git repo, sets the workspace root to the parent
    directory so all repos (including the current one) are equal children.

    Creates .hive/ directories if missing.
    Updates workspace.yaml with repo mappings.

    Exit codes:
    - 0: Success
    - 2: gh CLI missing or not authenticated
    - 3: Invalid workspace
    """
    if org is None:
        _init_interactive(
            ctx, name=name, description=description, dest=dest,
            ssh=ssh, https=https, json_output=json_output,
            match=match, exclude=exclude, visibility=visibility,
            fetch=fetch,
        )
    else:
        _init_from_org(
            ctx, org=org, name=name, description=description,
            dest=dest, match=match, exclude=exclude,
            visibility=visibility, fetch=fetch, ssh=ssh,
            https=https, json_output=json_output,
        )


def _init_interactive(
    ctx: typer.Context,
    *,
    name: str | None,
    description: str | None,
    dest: str | None,
    ssh: bool,
    https: bool,
    json_output: bool,
    match: str | None,
    exclude: str | None,
    visibility: str,
    fetch: bool,
) -> None:
    """Interactive init flow: detect local repos, optionally clone from org."""
    if json_output:
        _error("--org is required when using --json", json_output, exit_code=2)
        return

    # Get context from global options
    root = ctx.obj.get("root") if ctx.obj else None
    no_color = ctx.obj.get("no_color", False) if ctx.obj else False
    verbose = ctx.obj.get("verbose", False) if ctx.obj else False

    # If inside a git repo, workspace root is the parent directory
    cwd = Path.cwd()
    if is_git_repo(cwd) and root is None and dest is None:
        root = str(cwd.parent)
        typer.echo(f"Detected git repo: {cwd.name}")
        typer.echo(f"Workspace root: {cwd.parent}")

    context = get_context(root=root, no_color=no_color, verbose=verbose)

    dest_path = context.root
    if dest:
        dest_path = Path(dest)
        if not dest_path.is_absolute():
            dest_path = (context.root / dest_path).resolve()

    # Scan for local repos
    local_repos = _scan_local_repos(dest_path)

    workspace_repos: dict[str, str] = {}

    if local_repos:
        typer.echo(f"\nDetected {len(local_repos)} repo(s):")
        for i, repo in enumerate(local_repos, 1):
            typer.echo(f"  {i}. {repo['name']}")

        include_all = typer.confirm("\nInclude all?", default=True)
        if include_all:
            selected = local_repos
        else:
            selection = typer.prompt(
                "Enter repo numbers to include (comma-separated)",
                default=",".join(str(i) for i in range(1, len(local_repos) + 1)),
            )
            indices = [int(s.strip()) for s in selection.split(",") if s.strip().isdigit()]
            selected = [local_repos[i - 1] for i in indices if 1 <= i <= len(local_repos)]

        for repo in selected:
            repo_key = _repo_name_to_key(repo["name"])
            try:
                rel = repo["path"].relative_to(context.root)
                workspace_repos[repo_key] = f"./{rel}"
            except ValueError:
                workspace_repos[repo_key] = str(repo["path"])
    else:
        typer.echo("No git repos found in child directories.")

    # Optionally clone from a GitHub org
    org_input = typer.prompt("\nGitHub org to clone more repos from (blank to skip)", default="")
    if org_input:
        if not check_gh_auth():
            _error("gh CLI not authenticated. Run: gh auth login", False, exit_code=2)
            return

        use_ssh = ssh and not https
        context.ensure_dirs()
        dest_path.mkdir(parents=True, exist_ok=True)

        typer.echo(f"Fetching repos from {org_input}...")
        try:
            org_repos = list_org_repos(org_input, visibility=visibility)
        except GhError as e:
            _error(str(e), False, exit_code=2)
            return

        filtered = _filter_repos(org_repos, match=match, exclude=exclude)
        # Filter out repos already local
        local_names = {r["name"] for r in local_repos} if local_repos else set()
        missing = [r for r in filtered if r["name"] not in local_names]

        if missing:
            typer.echo(f"\n{len(missing)} repo(s) not yet local:")
            for i, repo in enumerate(missing, 1):
                typer.echo(f"  {i}. {repo['name']}")

            clone_all = typer.confirm("\nClone all?", default=True)
            if clone_all:
                to_clone = missing
            else:
                selection = typer.prompt(
                    "Enter repo numbers to clone (comma-separated)",
                    default=",".join(str(i) for i in range(1, len(missing) + 1)),
                )
                indices = [int(s.strip()) for s in selection.split(",") if s.strip().isdigit()]
                to_clone = [missing[i - 1] for i in indices if 1 <= i <= len(missing)]

            for repo in to_clone:
                result = _process_repo(
                    repo=repo,
                    dest_path=dest_path,
                    use_ssh=use_ssh,
                    do_fetch=fetch,
                    verbose=verbose,
                )
                if result["status"] in ("cloned", "exists", "fetched"):
                    repo_key = _repo_name_to_key(repo["name"])
                    workspace_repos[repo_key] = f"./{repo['name']}"
                    if result["status"] == "cloned":
                        typer.secho(f"  Cloned {repo['name']}", fg="green")
                elif result["status"] == "error":
                    typer.secho(f"  Failed {repo['name']}: {result.get('error')}", fg="red")
        else:
            typer.echo("All org repos already present locally.")

    if not workspace_repos:
        _error("No repos selected. Nothing to do.", False, exit_code=3)
        return

    # Prompt for issues repo
    defaults: dict[str, Any] = {}
    provider: dict[str, Any] = {}
    repo_keys = list(workspace_repos.keys())
    typer.echo(f"\nSelected repos: {', '.join(repo_keys)}")
    issues_repo_key = typer.prompt(
        "Issues repo key (for umbrella issues, blank to skip)", default=""
    )
    if issues_repo_key:
        if issues_repo_key in workspace_repos:
            defaults["issues_repo"] = issues_repo_key
            # Auto-resolve provider.repo to OWNER/REPO format
            repo_path = (context.root / workspace_repos[issues_repo_key]).resolve()
            if repo_path.is_dir():
                github_path = _resolve_github_repo(repo_path)
                if github_path:
                    provider["type"] = "github"
                    provider["repo"] = github_path
                    typer.echo(f"  Issues repo: {github_path}")
        else:
            typer.secho(
                f"  '{issues_repo_key}' is not a selected repo, skipping.",
                fg="yellow",
            )

    # LLM repo descriptions
    repo_descriptions: dict[str, str] = {}
    if typer.confirm("Describe repos using Claude?", default=True):
        typer.echo("Generating descriptions...")
        repo_descriptions = _describe_repos_with_llm(workspace_repos, context.root)
        if repo_descriptions:
            for key, desc in repo_descriptions.items():
                typer.echo(f"  {key}: {desc}")
        else:
            typer.echo("  Could not generate descriptions, using placeholders.")

    # Prompt for project name
    default_name = name or context.root.name
    project_name = typer.prompt("Project name", default=default_name)
    project_desc = description or "A multi-repo project managed with hive."

    # Create workspace
    context.ensure_dirs()

    from hive.core.defaults import ensure_default_files

    created_defaults = ensure_default_files(context.hive_dir)
    if created_defaults:
        typer.echo(f"Created defaults: {', '.join(created_defaults)}")

    workspace_path = context.workspace_yaml
    if workspace_path.exists():
        update_workspace_repos(workspace_path, workspace_repos)
    else:
        create_workspace_yaml(
            workspace_path, workspace_repos,
            defaults=defaults or None, provider=provider or None,
        )
    typer.echo(f"\nWorkspace updated: {workspace_path}")

    # Generate CLAUDE.md
    claude_md_path = context.root / "CLAUDE.md"
    if not claude_md_path.exists():
        org_label = org_input or project_name
        _generate_claude_md(
            claude_md_path, project_name, project_desc,
            workspace_repos, org_label,
            descriptions=repo_descriptions or None,
        )
        typer.echo(f"Generated: {claude_md_path}")
        if not repo_descriptions:
            typer.echo("  Edit CLAUDE.md to add repo descriptions and project details.")


def _init_from_org(
    ctx: typer.Context,
    *,
    org: str,
    name: str | None,
    description: str | None,
    dest: str | None,
    match: str | None,
    exclude: str | None,
    visibility: str,
    fetch: bool,
    ssh: bool,
    https: bool,
    json_output: bool,
) -> None:
    """Original org-based init flow."""
    # Get context from global options
    root = ctx.obj.get("root") if ctx.obj else None
    no_color = ctx.obj.get("no_color", False) if ctx.obj else False
    verbose = ctx.obj.get("verbose", False) if ctx.obj else False

    # If inside a git repo, workspace root is the parent directory
    cwd = Path.cwd()
    if is_git_repo(cwd) and root is None and dest is None:
        root = str(cwd.parent)
        if not json_output:
            typer.echo(f"Detected git repo: {cwd.name}")
            typer.echo(f"Workspace root: {cwd.parent}")

    context = get_context(root=root, no_color=no_color, verbose=verbose)

    # Determine clone URL type (--https overrides --ssh)
    use_ssh = ssh and not https

    # Determine destination directory for cloning
    if dest:
        dest_path = Path(dest)
        if not dest_path.is_absolute():
            dest_path = (context.root / dest_path).resolve()
    else:
        dest_path = context.root

    # Check gh auth
    if not check_gh_auth():
        _error("gh CLI not authenticated. Run: gh auth login", json_output, exit_code=2)
        return

    # Create required directories
    context.ensure_dirs()

    from hive.core.defaults import ensure_default_files

    ensure_default_files(context.hive_dir)

    dest_path.mkdir(parents=True, exist_ok=True)

    # Fetch repo list from GitHub
    if not json_output:
        typer.echo(f"Fetching repos from {org}...")

    try:
        repos = list_org_repos(org, visibility=visibility)
    except GhError as e:
        _error(str(e), json_output, exit_code=2)
        return

    # Filter repos
    filtered_repos = _filter_repos(repos, match=match, exclude=exclude)

    if not filtered_repos:
        _error(f"No repos found matching filters in {org}", json_output, exit_code=3)
        return

    if not json_output:
        typer.echo(f"Found {len(filtered_repos)} repos to process")

    # Process each repo
    results: list[dict[str, Any]] = []
    workspace_repos: dict[str, str] = {}

    for repo in filtered_repos:
        result = _process_repo(
            repo=repo,
            dest_path=dest_path,
            use_ssh=use_ssh,
            do_fetch=fetch,
            verbose=verbose and not json_output,
        )
        results.append(result)

        if result["status"] in ("cloned", "exists", "fetched"):
            repo_key = _repo_name_to_key(repo["name"])
            rel_path = f"./{repo['name']}"
            workspace_repos[repo_key] = rel_path

    # Update workspace.yaml
    if workspace_repos:
        workspace_path = context.workspace_yaml
        if workspace_path.exists():
            update_workspace_repos(workspace_path, workspace_repos)
        else:
            create_workspace_yaml(workspace_path, workspace_repos)

    # Generate CLAUDE.md
    claude_md_path = context.root / "CLAUDE.md"
    if not claude_md_path.exists():
        project_name = name or org
        project_desc = description or "A multi-repo project managed with hive."
        _generate_claude_md(claude_md_path, project_name, project_desc, workspace_repos, org)
        if not json_output:
            typer.echo(f"Generated: {claude_md_path}")
            typer.echo("  Edit CLAUDE.md to add repo descriptions and project details.")

    # Output results
    if json_output:
        output = {
            "org": org,
            "destination": str(dest_path),
            "repos_processed": len(results),
            "repos_cloned": sum(1 for r in results if r["status"] == "cloned"),
            "repos_skipped": sum(1 for r in results if r["status"] == "exists"),
            "repos_fetched": sum(1 for r in results if r["status"] == "fetched"),
            "repos_failed": sum(1 for r in results if r["status"] == "error"),
            "workspace_yaml": str(context.workspace_yaml),
            "results": results,
        }
        typer.echo(json.dumps(output, indent=2))
    else:
        _print_summary(results, context.workspace_yaml)


def _scan_local_repos(directory: Path) -> list[dict[str, Any]]:
    """Find git repos in child directories."""
    repos = []
    for child in sorted(directory.iterdir()):
        if child.is_dir() and not child.name.startswith(".") and is_git_repo(child):
            repos.append({"name": child.name, "path": child})
    return repos


def _filter_repos(
    repos: list[dict[str, Any]],
    match: str | None = None,
    exclude: str | None = None,
) -> list[dict[str, Any]]:
    """Filter repos by name patterns, excluding archived and forks."""
    filtered = []

    # Compile regex patterns
    match_re = re.compile(match) if match else None
    exclude_re = re.compile(exclude) if exclude else None

    for repo in repos:
        name = repo["name"]

        # Skip archived repos
        if repo.get("isArchived", False):
            continue

        # Skip forks
        if repo.get("isFork", False):
            continue

        # Apply match filter
        if match_re and not match_re.search(name):
            continue

        # Apply exclude filter
        if exclude_re and exclude_re.search(name):
            continue

        filtered.append(repo)

    return filtered


def _process_repo(
    repo: dict[str, Any],
    dest_path: Path,
    use_ssh: bool,
    do_fetch: bool,
    verbose: bool,
) -> dict[str, Any]:
    """Process a single repo (clone or fetch)."""
    name = repo["name"]
    repo_path = dest_path / name

    result: dict[str, Any] = {
        "name": name,
        "path": str(repo_path),
    }

    # Get clone URL
    clone_url = repo.get("sshUrl") if use_ssh else repo.get("url")
    if not clone_url:
        result["status"] = "error"
        result["error"] = "No clone URL available"
        return result

    if repo_path.exists():
        if is_git_repo(repo_path):
            if do_fetch:
                # Fetch existing repo
                if verbose:
                    typer.echo(f"  Fetching {name}...")
                try:
                    fetch_origin(repo_path, prune=True)
                    result["status"] = "fetched"
                except GitError as e:
                    result["status"] = "error"
                    result["error"] = str(e)
            else:
                if verbose:
                    typer.echo(f"  Skipping {name} (exists)")
                result["status"] = "exists"
        else:
            result["status"] = "error"
            result["error"] = "Path exists but is not a git repo"
    else:
        # Clone repo
        if verbose:
            typer.echo(f"  Cloning {name}...")
        try:
            clone_repo(clone_url, repo_path)
            result["status"] = "cloned"
        except GitError as e:
            result["status"] = "error"
            result["error"] = str(e)

    return result


def _resolve_github_repo(repo_path: Path) -> str | None:
    """Get OWNER/REPO from a local git repo's origin remote.

    Returns None if the remote can't be parsed.
    """
    try:
        result = subprocess.run(
            ["git", "remote", "get-url", "origin"],
            cwd=repo_path,
            capture_output=True,
            text=True,
            timeout=10,
        )
        if result.returncode == 0:
            url = result.stdout.strip()
            if "github.com" in url:
                if url.startswith("git@"):
                    path = url.split(":")[-1]
                else:
                    path = url.split("github.com/")[-1]
                return path.removesuffix(".git")
    except (subprocess.TimeoutExpired, OSError):
        pass
    return None


def _repo_name_to_key(name: str) -> str:
    """Convert repo name to a valid workspace key.

    Converts hyphens to underscores for Python-friendly keys.
    """
    return name.replace("-", "_")


def _print_summary(results: list[dict[str, Any]], workspace_path: Path) -> None:
    """Print human-readable summary."""
    cloned = sum(1 for r in results if r["status"] == "cloned")
    skipped = sum(1 for r in results if r["status"] == "exists")
    fetched = sum(1 for r in results if r["status"] == "fetched")
    failed = sum(1 for r in results if r["status"] == "error")

    typer.echo()
    typer.echo("Summary:")
    if cloned:
        typer.secho(f"  Cloned: {cloned}", fg="green")
    if fetched:
        typer.secho(f"  Fetched: {fetched}", fg="cyan")
    if skipped:
        typer.echo(f"  Skipped (existing): {skipped}")
    if failed:
        typer.secho(f"  Failed: {failed}", fg="red")
        for r in results:
            if r["status"] == "error":
                typer.echo(f"    {r['name']}: {r.get('error', 'unknown error')}")

    typer.echo()
    typer.echo(f"Workspace updated: {workspace_path}")


def _error(message: str, json_output: bool, exit_code: int) -> None:
    """Output error and exit."""
    if json_output:
        typer.echo(json.dumps({"error": message}))
    else:
        typer.secho(f"Error: {message}", fg="red", err=True)
    raise typer.Exit(exit_code)


def _describe_repos_with_llm(
    repos: dict[str, str],
    root: Path,
) -> dict[str, str]:
    """Use claude -p to generate one-sentence descriptions for each repo.

    Args:
        repos: Mapping of repo key to relative path.
        root: Workspace root directory.

    Returns:
        Mapping of repo key to description string. Empty dict on failure.
    """
    # Gather context for each repo
    repo_contexts = []
    for key, rel_path in repos.items():
        repo_path = (root / rel_path).resolve()
        context_parts = [f"## {key} ({rel_path})"]

        # Read first 100 lines of README.md if it exists
        for readme_name in ("README.md", "readme.md", "README.rst", "README"):
            readme_path = repo_path / readme_name
            if readme_path.is_file():
                try:
                    lines = readme_path.read_text().splitlines()[:100]
                    context_parts.append("README (first 100 lines):")
                    context_parts.append("\n".join(lines))
                except OSError:
                    pass
                break

        # List top-level files
        try:
            entries = sorted(p.name for p in repo_path.iterdir() if not p.name.startswith("."))
            context_parts.append(f"Top-level files: {', '.join(entries)}")
        except OSError:
            pass

        repo_contexts.append("\n".join(context_parts))

    prompt = (
        "For each repository below, write exactly one line in the format:\n"
        "repo_key: one-sentence description\n\n"
        "Do not include any other text, headings, or formatting.\n\n"
        + "\n\n".join(repo_contexts)
    )

    try:
        result = subprocess.run(
            ["claude", "-p", prompt],
            capture_output=True,
            text=True,
            timeout=60,
        )
        if result.returncode != 0:
            return {}

        descriptions: dict[str, str] = {}
        for line in result.stdout.strip().splitlines():
            if ": " in line:
                key, _, desc = line.partition(": ")
                key = key.strip()
                if key in repos:
                    descriptions[key] = desc.strip()
        return descriptions
    except (FileNotFoundError, subprocess.TimeoutExpired, OSError):
        return {}


def _generate_claude_md(
    path: Path,
    project_name: str,
    project_description: str,
    repos: dict[str, str],
    org: str,
    descriptions: dict[str, str] | None = None,
) -> None:
    """Generate CLAUDE.md with project context."""
    descriptions = descriptions or {}
    repo_list = "\n".join(
        f"- **{key}**: `{path}` - {descriptions.get(key, '[describe what this repo does]')}"
        for key, path in repos.items()
    )

    content = f'''# {project_name}

{project_description}

## Repositories

This workspace contains the following repositories:

{repo_list}

## Hive Workflow

This project uses **hive** for multi-repo coordination. GitHub Issues are the source of truth.

### Quest Commands

```bash
# Quest lifecycle
hive plan "title"         # Create a quest with objectives and plan
hive status               # Show active quest status, signals, and progress
hive next                 # Get next recommended action based on quest state
hive heartbeat            # Poll watchers for signals and update quest state
hive ask "question"       # Ask a question about the current quest
```

### Task Execution

```bash
# Planning & Coordination
hive up                   # Start hive tmux session (Claude + dashboard)
hive pick start           # Interactively select an issue and start working

# Issue Management
hive issue new "title" --repos repo1,repo2   # Create umbrella + repo issues
hive issue list           # List local tasks
hive issue sync <id>      # Sync umbrella issue status

# Working on Tasks
hive start <id>           # Create worktrees and tmux windows for a task
hive start <id> --repos x # Start only specific repos
hive clean <id> --yes     # Remove worktrees for a task
```

### Workflow

1. **Plan quest**: `hive plan "Feature X"` to define objectives and create a quest
2. **Start work**: `hive start <issue_number>` to create worktrees and branches
3. **Monitor signals**: `hive heartbeat` to poll watchers (Sentry, Fastmail) for alerts
4. **Review status**: `hive status` to see quest progress, signals, and next steps
5. **Iterate**: `hive next` for recommended actions, `hive ask` for guidance

### Architecture Notes

- **Quests**: High-level goals tracked in `.hive/state.json`
- **Signals**: External events (errors, alerts) ingested by watchers via `hive heartbeat`
- **Umbrella Issue**: Created in the issues repo, links to all repo-specific issues
- **Worktrees**: Created in `.hive/wt/<task_id>/<repo>/`
- **Branches**: Named `feat/<task_id>-<repo>`

### Watcher Configuration (Optional)

Watchers poll external services for signals during `hive heartbeat`:

- **Sentry**: Set `SENTRY_AUTH_TOKEN`, `SENTRY_ORG`, `SENTRY_PROJECT`
- **Fastmail**: Set `FASTMAIL_USER`, `FASTMAIL_APP_PASSWORD`

## Project-Specific Notes

[Add any project-specific context, conventions, or guidelines here]

---
*Generated by hive init. Edit this file to add project details.*
'''

    with open(path, "w") as f:
        f.write(content)
