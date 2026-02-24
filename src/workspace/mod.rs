//! Workspace configuration — loaded from `workspace.yaml` or `.hive/workspace.yaml`.
//!
//! The workspace config tells hive about the repos it manages, the default
//! agent to use, team conventions, and any soul/memory text for the coordinator.

use color_eyre::eyre::{Result, WrapErr};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Workspace configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Workspace {
    /// Human-readable workspace name.
    #[serde(default = "default_name")]
    pub name: String,

    /// List of repositories this workspace manages.
    /// Each entry can be a path or "owner/repo" for GitHub repos.
    #[serde(default)]
    pub repos: Vec<String>,

    /// Default agent kind to use (e.g. "claude", "codex").
    #[serde(default = "default_agent")]
    pub default_agent: String,

    /// Soul text — personality and context for the coordinator agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub soul: Option<String>,

    /// Team conventions and coding standards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub conventions: Option<String>,

    /// The root directory where the workspace config was found.
    #[serde(skip)]
    pub root: PathBuf,
}

fn default_name() -> String {
    "apiari".to_owned()
}

fn default_agent() -> String {
    "claude".to_owned()
}

impl Default for Workspace {
    fn default() -> Self {
        Self {
            name: default_name(),
            repos: Vec::new(),
            default_agent: default_agent(),
            soul: None,
            conventions: None,
            root: PathBuf::from("."),
        }
    }
}

/// Candidate file names for workspace config, in priority order.
const CONFIG_CANDIDATES: &[&str] = &[
    "workspace.yaml",
    "workspace.yml",
    ".hive/workspace.yaml",
    ".hive/workspace.yml",
];

/// Load the workspace configuration.
///
/// Searches upward from `start_dir` for a workspace config file. If none
/// is found, returns a default workspace rooted at `start_dir`.
pub fn load_workspace(start_dir: &Path) -> Result<Workspace> {
    let mut dir = start_dir.to_path_buf();

    loop {
        for candidate in CONFIG_CANDIDATES {
            let path = dir.join(candidate);
            if path.exists() {
                return load_from_file(&path, &dir);
            }
        }

        if !dir.pop() {
            break;
        }
    }

    // No config found; return a default workspace rooted at start_dir.
    eprintln!(
        "No workspace config found; using defaults (root: {})",
        start_dir.display()
    );
    Ok(Workspace {
        root: start_dir.to_path_buf(),
        ..Workspace::default()
    })
}

/// Load a workspace from a specific YAML file.
fn load_from_file(path: &Path, root: &Path) -> Result<Workspace> {
    let contents = std::fs::read_to_string(path)
        .wrap_err_with(|| format!("failed to read {}", path.display()))?;

    let mut workspace: Workspace = serde_yaml::from_str(&contents)
        .wrap_err_with(|| format!("failed to parse {}", path.display()))?;

    workspace.root = root.to_path_buf();

    // If .hive/soul.md exists, use it instead of the inline soul field.
    let soul_path = root.join(".hive/soul.md");
    if let Ok(soul) = std::fs::read_to_string(&soul_path)
        && !soul.trim().is_empty()
    {
        workspace.soul = Some(soul);
    }

    Ok(workspace)
}

/// Initialize a new workspace by creating a default config file.
///
/// Writes `.hive/workspace.yaml` in the given directory.
pub fn init_workspace(dir: &Path) -> Result<PathBuf> {
    let hive_dir = dir.join(".hive");
    std::fs::create_dir_all(&hive_dir).wrap_err("failed to create .hive directory")?;

    let config_path = hive_dir.join("workspace.yaml");
    if config_path.exists() {
        eprintln!("Workspace config already exists: {}", config_path.display());
        return Ok(config_path);
    }

    let default = Workspace {
        name: dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace")
            .to_owned(),
        root: dir.to_path_buf(),
        ..Workspace::default()
    };

    let yaml =
        serde_yaml::to_string(&default).wrap_err("failed to serialize default workspace config")?;
    std::fs::write(&config_path, yaml).wrap_err("failed to write workspace config")?;

    // Also create the quests directory.
    let quests_dir = hive_dir.join("quests");
    std::fs::create_dir_all(&quests_dir).wrap_err("failed to create quests directory")?;

    Ok(config_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn workspace_default_has_sensible_values() {
        let w = Workspace::default();
        assert_eq!(w.name, "apiari");
        assert_eq!(w.default_agent, "claude");
        assert!(w.repos.is_empty());
        assert!(w.soul.is_none());
        assert!(w.conventions.is_none());
    }

    #[test]
    fn load_workspace_falls_back_to_default_when_no_config() {
        let dir = TempDir::new().unwrap();
        let ws = load_workspace(dir.path()).unwrap();
        assert_eq!(ws.name, "apiari");
        assert_eq!(ws.root, dir.path());
    }

    #[test]
    fn load_workspace_finds_workspace_yaml() {
        let dir = TempDir::new().unwrap();
        let yaml =
            "name: myproject\nrepos:\n  - owner/repo1\n  - owner/repo2\ndefault_agent: codex\n";
        std::fs::write(dir.path().join("workspace.yaml"), yaml).unwrap();
        let ws = load_workspace(dir.path()).unwrap();
        assert_eq!(ws.name, "myproject");
        assert_eq!(ws.repos, vec!["owner/repo1", "owner/repo2"]);
        assert_eq!(ws.default_agent, "codex");
        assert_eq!(ws.root, dir.path());
    }

    #[test]
    fn load_workspace_finds_hive_workspace_yaml() {
        let dir = TempDir::new().unwrap();
        let hive_dir = dir.path().join(".hive");
        std::fs::create_dir_all(&hive_dir).unwrap();
        std::fs::write(hive_dir.join("workspace.yaml"), "name: hive-project\n").unwrap();
        let ws = load_workspace(dir.path()).unwrap();
        assert_eq!(ws.name, "hive-project");
    }

    #[test]
    fn load_workspace_walks_up_to_parent() {
        let dir = TempDir::new().unwrap();
        std::fs::write(dir.path().join("workspace.yaml"), "name: parentproject\n").unwrap();
        let sub = dir.path().join("sub").join("deep");
        std::fs::create_dir_all(&sub).unwrap();
        let ws = load_workspace(&sub).unwrap();
        assert_eq!(ws.name, "parentproject");
    }

    #[test]
    fn load_workspace_prefers_workspace_yaml_over_hive_yaml() {
        let dir = TempDir::new().unwrap();
        let hive_dir = dir.path().join(".hive");
        std::fs::create_dir_all(&hive_dir).unwrap();
        std::fs::write(dir.path().join("workspace.yaml"), "name: top-level\n").unwrap();
        std::fs::write(hive_dir.join("workspace.yaml"), "name: hive-level\n").unwrap();
        let ws = load_workspace(dir.path()).unwrap();
        assert_eq!(ws.name, "top-level");
    }

    #[test]
    fn init_workspace_creates_structure() {
        let dir = TempDir::new().unwrap();
        let config_path = init_workspace(dir.path()).unwrap();
        assert!(config_path.exists());
        assert!(dir.path().join(".hive").join("workspace.yaml").exists());
        assert!(dir.path().join(".hive").join("quests").exists());
    }

    #[test]
    fn init_workspace_uses_dir_name() {
        let dir = TempDir::new().unwrap();
        init_workspace(dir.path()).unwrap();
        let ws = load_workspace(dir.path()).unwrap();
        let expected_name = dir.path().file_name().unwrap().to_str().unwrap();
        assert_eq!(ws.name, expected_name);
    }

    #[test]
    fn init_workspace_idempotent() {
        let dir = TempDir::new().unwrap();
        let path1 = init_workspace(dir.path()).unwrap();
        let path2 = init_workspace(dir.path()).unwrap();
        assert_eq!(path1, path2);
    }
}
