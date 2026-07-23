use std::path::Path;

use crate::adapter::config::ConfigLoader;
use crate::adapter::filesystem;
use crate::adapter::git::GitCli;
use crate::adapter::sqlite::ProjectDatabase;
use crate::adapter::sqlite_repos::SqliteEventRepository;
use crate::adapter::xdg::XdgPaths;
use crate::domain::config::CarryCtxConfig;
use crate::domain::ids;
use crate::error::CarryCtxError;
use crate::repository::event::{EventRepository, NewEvent};

/// Result of a successful initialization
#[derive(serde::Serialize)]
pub struct InitResult {
    pub project_id: String,
    pub project_name: String,
    pub task_prefix: String,
    pub config_path: String,
    pub state_path: String,
    pub created: Vec<String>,
}

/// Initialize a CarryCtx project in the working directory.
///
/// Flow:
/// 1. Discover Git repository from start_path
/// 2. Load configuration (global + project)
/// 3. Check for existing state
/// 4. Create .carryctx directory with config.toml and README.md
/// 5. Initialize the project database in git-common-dir
/// 6. Register in the global registry
/// 7. Append project.initialized event
pub fn init_project(
    start_path: &Path,
    name: Option<&str>,
    task_prefix: Option<&str>,
    force: bool,
) -> Result<InitResult, CarryCtxError> {
    let xdg = XdgPaths::new();
    let git = GitCli::new();
    let git_project = git.discover(start_path)?;
    let repository_root = &git_project.repository_root;
    let git_common_dir = &git_project.git_common_dir;

    // Paths
    let carryctx_dir = repository_root.join(".carryctx");
    let config_path = carryctx_dir.join("config.toml");
    let readme_path = carryctx_dir.join("README.md");
    let state_dir = xdg.project_state_dir(git_common_dir);
    let state_path = xdg.project_db(git_common_dir);
    let registry_path = xdg.registry_db();

    // Load existing config if present
    let cfg_loader = ConfigLoader::new(XdgPaths::new());
    let existing_config = if config_path.exists() {
        cfg_loader.load(Some(repository_root)).ok()
    } else {
        None
    };

    // Check for existing state
    let existing = if state_path.exists() {
        match ProjectDatabase::open_readonly(&state_path) {
            Ok(db) => {
                let project = get_project_from_db(&db);
                drop(db);
                project
            }
            Err(_) => None,
        }
    } else {
        None
    };

    if let Some(ref project) = existing {
        if !force {
            return Err(CarryCtxError::state_conflict(format!(
                "Project '{}' is already initialized at {}.",
                project.name,
                repository_root.display()
            )));
        }
    }

    // Determine project identity
    let prefix = task_prefix
        .or_else(|| {
            existing_config
                .as_ref()
                .map(|c| c.project.task_prefix.as_str())
        })
        .unwrap_or("CTX")
        .to_string();

    let project_name = name
        .or_else(|| existing_config.as_ref().map(|c| c.project.name.as_str()))
        .map(|s| s.to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            repository_root
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "project".into())
        });

    let project_id = existing_config
        .as_ref()
        .map(|c| c.project.id.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ids::new_internal_id().to_string());

    let now = chrono::Utc::now().to_rfc3339();

    // Create .carryctx directory
    filesystem::ensure_dir(&carryctx_dir)?;

    // Write config.toml
    let config_content = build_config_toml(&project_id, &project_name, &prefix, &git_project);
    filesystem::write_atomic(&config_path, config_content.as_bytes())?;

    // Write README.md
    if !readme_path.exists() {
        let readme_content = [
            "# CarryCtx\n",
            "\n",
            "<!-- carryctx:v1 -->\n",
            "\n",
            "This directory contains versioned CarryCtx project configuration.\n",
            "Runtime state is stored in the repository's Git common directory, not here.\n",
        ]
        .concat();
        filesystem::write_atomic(&readme_path, readme_content.as_bytes())?;
    }

    // Ensure .gitignore has .carryctx/config.local.toml
    let gitignore_path = repository_root.join(".gitignore");
    ensure_gitignore_rule(&gitignore_path)?;

    // Create state directory and database
    filesystem::ensure_dir(&state_dir)?;
    let mut db = ProjectDatabase::create_fresh(&state_path)?;

    // Insert project row
    let main_branch = git_project.branch.as_deref().unwrap_or("main");
    db.connection_mut()
        .execute(
            "INSERT OR REPLACE INTO projects (id, name, task_prefix, repository_root, git_common_dir, main_branch, schema_version, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 4, ?7, ?7)",
            rusqlite::params![
                project_id,
                project_name,
                prefix,
                repository_root.to_string_lossy().as_ref(),
                git_common_dir.to_string_lossy().as_ref(),
                main_branch,
                now,
            ],
        )
        .map_err(|e| CarryCtxError::database_error(format!("Failed to insert project: {e}")))?;

    // Append project.initialized event
    let event_repo = SqliteEventRepository::new(db.connection());
    event_repo.append(&NewEvent {
        id: ulid::Ulid::generate().to_string(),
        project_id: project_id.clone(),
        event_type: "project.initialized".into(),
        actor_agent_id: None,
        session_id: None,
        task_id: None,
        payload: serde_json::json!({
            "source": "init",
            "schemaVersion": 4,
        }),
        occurred_at: now.clone(),
    })?;

    // Register in global registry
    register_in_registry(
        &registry_path,
        &project_id,
        repository_root,
        git_common_dir,
        &config_path,
        &now,
    )?;

    let mut created = Vec::new();
    created.push("state".into());
    created.push("config".into());
    created.push("project".into());
    if !registry_path.exists() {
        created.push("registry".into());
    }

    Ok(InitResult {
        project_id,
        project_name,
        task_prefix: prefix,
        config_path: config_path.to_string_lossy().to_string(),
        state_path: state_path.to_string_lossy().to_string(),
        created,
    })
}

fn build_config_toml(
    project_id: &str,
    name: &str,
    task_prefix: &str,
    git: &crate::adapter::git::GitProject,
) -> String {
    let config = CarryCtxConfig {
        schema_version: 1,
        project: crate::domain::config::ProjectConfig {
            id: project_id.into(),
            name: name.into(),
            task_prefix: task_prefix.into(),
        },
        git: crate::domain::config::GitConfig {
            main_branch: git.branch.as_deref().unwrap_or("main").into(),
            ..Default::default()
        },
        ..Default::default()
    };
    toml::to_string_pretty(&config).unwrap_or_default()
}

fn ensure_gitignore_rule(gitignore_path: &Path) -> Result<(), CarryCtxError> {
    let rules = vec![".carryctx/config.local.toml", ".worktrees/"];
    
    if gitignore_path.exists() {
        let content = std::fs::read_to_string(gitignore_path).map_err(|e| {
            CarryCtxError::resource_not_found(format!("Cannot read .gitignore: {e}"))
        })?;
        
        let mut amended = content.clone();
        if !amended.ends_with('\n') && !amended.is_empty() {
            amended.push('\n');
        }
        
        let mut changed = false;
        for rule in rules {
            if !content.lines().any(|l| l.trim() == rule) {
                amended.push_str(rule);
                amended.push('\n');
                changed = true;
            }
        }
        
        if changed {
            std::fs::write(gitignore_path, amended).map_err(|e| {
                CarryCtxError::database_error(format!("Failed to write .gitignore: {e}"))
            })?;
        }
    } else {
        let content = format!("{}\n{}\n", rules[0], rules[1]);
        std::fs::write(gitignore_path, content).map_err(|e| {
            CarryCtxError::database_error(format!("Failed to create .gitignore: {e}"))
        })?;
    }
    Ok(())
}

fn register_in_registry(
    registry_path: &Path,
    project_id: &str,
    repository_root: &Path,
    git_common_dir: &Path,
    config_path: &Path,
    now: &str,
) -> Result<(), CarryCtxError> {
    let registry_dir = registry_path.parent().unwrap_or(Path::new("."));
    filesystem::ensure_dir(registry_dir)?;

    // Simple JSON-based registry
    let mut registry: Vec<serde_json::Value> = if registry_path.exists() {
        let content = std::fs::read_to_string(registry_path).unwrap_or_else(|_| "[]".into());
        serde_json::from_str(&content).unwrap_or_default()
    } else {
        Vec::new()
    };

    let entry = serde_json::json!({
        "id": project_id,
        "repositoryRoot": repository_root.to_string_lossy(),
        "gitCommonDir": git_common_dir.to_string_lossy(),
        "configPath": config_path.to_string_lossy(),
        "lastSeenAt": now,
    });

    // Replace existing entry or append
    if let Some(pos) = registry.iter().position(|e| e["id"] == project_id) {
        registry[pos] = entry;
    } else {
        registry.push(entry);
    }

    let json = serde_json::to_string_pretty(&registry)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to serialize registry: {e}")))?;
    std::fs::write(registry_path, json)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to write registry: {e}")))?;

    Ok(())
}

struct ProjectBrief {
    name: String,
}

fn get_project_from_db(db: &ProjectDatabase) -> Option<ProjectBrief> {
    let conn = db.connection();
    conn.query_row("SELECT name FROM projects LIMIT 1", [], |row| {
        let name: String = row.get(0)?;
        Ok(ProjectBrief { name })
    })
    .ok()
}
