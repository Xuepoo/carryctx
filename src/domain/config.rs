/// Full CarryCtx configuration model (mirrors TOML structure)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CarryCtxConfig {
    #[serde(default = "default_schema_version")]
    pub schema_version: u64,

    #[serde(default)]
    pub project: ProjectConfig,

    #[serde(default)]
    pub git: GitConfig,

    #[serde(default)]
    pub worktree: WorktreeConfig,

    #[serde(default)]
    pub session: SessionConfig,

    #[serde(default)]
    pub task: TaskConfig,

    #[serde(default)]
    pub context: ContextConfig,

    #[serde(default)]
    pub checkpoint: CheckpointConfig,

    #[serde(default)]
    pub output: OutputConfig,

    #[serde(default)]
    pub agent: AgentConfig,

    #[serde(default)]
    pub verification: VerificationConfig,
}

fn default_schema_version() -> u64 {
    1
}

impl Default for CarryCtxConfig {
    fn default() -> Self {
        Self {
            schema_version: 1,
            project: ProjectConfig::default(),
            git: GitConfig::default(),
            worktree: WorktreeConfig::default(),
            session: SessionConfig::default(),
            task: TaskConfig::default(),
            context: ContextConfig::default(),
            checkpoint: CheckpointConfig::default(),
            output: OutputConfig::default(),
            agent: AgentConfig::default(),
            verification: VerificationConfig::default(),
        }
    }
}

// --- Section configs ---

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProjectConfig {
    #[serde(default = "default_project_id")]
    pub id: String,

    #[serde(default)]
    pub name: String,

    #[serde(default = "default_task_prefix")]
    pub task_prefix: String,
}

fn default_project_id() -> String {
    "carryctx".into()
}
fn default_task_prefix() -> String {
    "CTX".into()
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            id: default_project_id(),
            name: String::new(),
            task_prefix: default_task_prefix(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GitConfig {
    #[serde(default = "default_main_branch")]
    pub main_branch: String,
    #[serde(default)]
    pub worktree_root: Option<String>,
    #[serde(default = "default_branch_template")]
    pub branch_template: String,
}

fn default_main_branch() -> String {
    "main".into()
}
fn default_branch_template() -> String {
    "carryctx/{task_id}-{slug}".into()
}

impl Default for GitConfig {
    fn default() -> Self {
        Self {
            main_branch: default_main_branch(),
            worktree_root: None,
            branch_template: default_branch_template(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct WorktreeConfig {
    #[serde(default)]
    pub cleanup: WorktreeCleanupConfig,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WorktreeCleanupConfig {
    #[serde(default = "default_cleanup_on_task_completed")]
    pub on_task_completed: String,
    #[serde(default = "default_cleanup_on_task_cancelled")]
    pub on_task_cancelled: String,
    #[serde(default = "default_true")]
    pub require_clean: bool,
    #[serde(default = "default_true")]
    pub require_no_active_session: bool,
    #[serde(default = "default_delete_branch")]
    pub delete_branch: String,
}

fn default_cleanup_on_task_completed() -> String {
    "when_idle".into()
}
fn default_cleanup_on_task_cancelled() -> String {
    "keep".into()
}
fn default_delete_branch() -> String {
    "never".into()
}

impl Default for WorktreeCleanupConfig {
    fn default() -> Self {
        Self {
            on_task_completed: default_cleanup_on_task_completed(),
            on_task_cancelled: default_cleanup_on_task_cancelled(),
            require_clean: true,
            require_no_active_session: true,
            delete_branch: default_delete_branch(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SessionConfig {
    #[serde(default = "default_stale_after")]
    pub stale_after: String,
    #[serde(default = "default_true")]
    pub single_active_session_per_agent: bool,
}

fn default_stale_after() -> String {
    "2h".into()
}
fn default_true() -> bool {
    true
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            stale_after: default_stale_after(),
            single_active_session_per_agent: default_true(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TaskConfig {
    #[serde(default = "default_true")]
    pub single_active_task_per_agent: bool,
    #[serde(default)]
    pub strict_completion: bool,
    /// Cap for task listings (`task list`); guards against unbounded scans
    /// over large projects. Override with `task list --limit`.
    #[serde(default = "default_task_list_limit")]
    pub list_limit: u64,
}

pub fn default_task_list_limit() -> u64 {
    200
}

impl Default for TaskConfig {
    fn default() -> Self {
        Self {
            single_active_task_per_agent: default_true(),
            strict_completion: false,
            list_limit: default_task_list_limit(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ContextConfig {
    #[serde(default = "default_context_mode")]
    pub default_mode: String,
    #[serde(default = "default_max_events")]
    pub max_events: u64,
    #[serde(default = "default_lookback")]
    pub lookback: String,
    #[serde(default = "default_true")]
    pub include_git_status: bool,
}

fn default_context_mode() -> String {
    "compact".into()
}
fn default_max_events() -> u64 {
    10
}
fn default_lookback() -> String {
    "7d".into()
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            default_mode: default_context_mode(),
            max_events: default_max_events(),
            lookback: default_lookback(),
            include_git_status: default_true(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CheckpointConfig {
    #[serde(default = "default_true")]
    pub require_before_session_end: bool,
    #[serde(default = "default_true")]
    pub capture_diff_stats: bool,
    #[serde(default = "default_true")]
    pub capture_untracked_files: bool,
}

impl Default for CheckpointConfig {
    fn default() -> Self {
        Self {
            require_before_session_end: default_true(),
            capture_diff_stats: default_true(),
            capture_untracked_files: default_true(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct OutputConfig {
    #[serde(default = "default_color")]
    pub color: String,
    #[serde(default = "default_true")]
    pub unicode: bool,
    /// Emit full detail text output (pretty-printed entity records) instead of
    /// compact one-line summaries. Equivalent to the global `--verbose` flag.
    #[serde(default)]
    pub verbose: bool,
    /// Per-command field allowlist applied to entity output (both text and
    /// JSON). Keys are command names (e.g. "task.list", "handoff.show"); the
    /// value is the list of record fields to keep. The global `--fields` flag
    /// overrides this for a single invocation.
    #[serde(default)]
    pub fields: std::collections::HashMap<String, Vec<String>>,
}

fn default_color() -> String {
    "auto".into()
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            color: default_color(),
            unicode: default_true(),
            verbose: false,
            fields: std::collections::HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct AgentConfig {
    pub default_name: Option<String>,
    pub default_provider: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct VerificationConfig {
    #[serde(default)]
    pub commands: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worktree_cleanup_defaults_are_fail_safe() {
        let config = WorktreeCleanupConfig::default();
        assert_eq!(config.on_task_completed, "when_idle");
        assert_eq!(config.on_task_cancelled, "keep");
        assert!(config.require_clean);
        assert!(config.require_no_active_session);
        assert_eq!(config.delete_branch, "never");
    }

    #[test]
    fn worktree_cleanup_config_round_trips_toml() {
        let config: CarryCtxConfig = toml::from_str(
            r#"[worktree.cleanup]
on_task_completed = "keep"
on_task_cancelled = "when_idle"
require_clean = false
require_no_active_session = false
delete_branch = "never"
"#,
        )
        .unwrap();
        assert_eq!(config.worktree.cleanup.on_task_completed, "keep");
        assert_eq!(config.worktree.cleanup.on_task_cancelled, "when_idle");
        assert!(!config.worktree.cleanup.require_clean);
        assert!(!config.worktree.cleanup.require_no_active_session);
    }
}
