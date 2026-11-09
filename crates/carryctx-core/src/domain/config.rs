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

    /// Global-only security gate. The project-level `[security]` table is
    /// ignored with a warning so a repository can never loosen or tighten the
    /// user's global posture (CTX-0100, design §9).
    #[serde(default)]
    pub security: SecurityConfig,

    /// Opt-in enforcement policies for role matching and active task concurrency (Issue #213).
    #[serde(default)]
    pub policy: PolicyConfig,
}

impl CarryCtxConfig {
    pub fn effective_policy(&self) -> PolicyConfig {
        if let Some(ref enf) = self.task.enforcement {
            PolicyConfig {
                enforce_role_matching: self.policy.enforce_role_matching
                    || enf.enforce_role_matching,
                single_active_task: self.policy.single_active_task || enf.single_active_task,
            }
        } else {
            self.policy.clone()
        }
    }

    /// Resolve the effective pre-completion verification commands and profile label
    /// for `carryctx.pre-task-complete` (CTX-0010, Issue #223).
    ///
    /// Returns `Ok(None)` when pre-completion verification is not active (default
    /// `strict = false` and no explicit profile or `[task.completion]` hook configured),
    /// or `Ok(Some((profile_name, commands)))` when verification commands should execute.
    /// Returns `Err(CONFIGURATION_ERROR)` if an explicitly requested or configured
    /// profile name does not exist in `[verification.profiles]`.
    pub fn resolve_pre_task_complete(
        &self,
        explicit_profile: Option<&str>,
    ) -> Result<Option<(String, Vec<String>)>, crate::error::CarryCtxError> {
        let comp = self.task.completion.as_ref();
        let strict_gate = self.verification.strict
            || self.verification.enforce_on_complete
            || comp.is_some_and(|c| c.strict);

        let requested_profile = explicit_profile
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .or_else(|| {
                comp.and_then(|c| c.profile.as_deref())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            })
            .or_else(|| {
                self.verification
                    .default_profile
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
            });

        if let Some(profile_name) = requested_profile {
            if let Some(profile) = self.verification.profiles.get(profile_name) {
                let cmds: Vec<String> = profile
                    .commands()
                    .iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if cmds.is_empty() {
                    return Ok(None);
                }
                return Ok(Some((profile_name.to_string(), cmds)));
            }
            if profile_name == "default" && !self.verification.commands.is_empty() {
                let cmds: Vec<String> = self
                    .verification
                    .commands
                    .iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !cmds.is_empty() {
                    return Ok(Some(("default".to_string(), cmds)));
                }
            }
            return Err(crate::error::CarryCtxError::configuration_error(format!(
                "Verification profile '{profile_name}' is not defined in [verification.profiles]."
            )));
        }

        if let Some(comp) = comp {
            let comp_cmds: Vec<String> = comp
                .commands
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !comp_cmds.is_empty() {
                return Ok(Some(("task.completion".to_string(), comp_cmds)));
            }
        }

        if strict_gate {
            if let Some(default_prof) = self.verification.profiles.get("default") {
                let cmds: Vec<String> = default_prof
                    .commands()
                    .iter()
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !cmds.is_empty() {
                    return Ok(Some(("default".to_string(), cmds)));
                }
            }
            let cmds: Vec<String> = self
                .verification
                .commands
                .iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect();
            if !cmds.is_empty() {
                return Ok(Some(("default".to_string(), cmds)));
            }
        }

        Ok(None)
    }
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
            security: SecurityConfig::default(),
            policy: PolicyConfig::default(),
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
    #[serde(deserialize_with = "deserialize_cleanup_policy")]
    pub on_task_completed: String,
    #[serde(default = "default_cleanup_on_task_cancelled")]
    #[serde(deserialize_with = "deserialize_cleanup_policy")]
    pub on_task_cancelled: String,
    #[serde(default = "default_true")]
    pub require_clean: bool,
    #[serde(default = "default_true")]
    pub require_no_active_session: bool,
    #[serde(default = "default_delete_branch")]
    #[serde(deserialize_with = "deserialize_delete_branch")]
    pub delete_branch: String,
}

fn deserialize_cleanup_policy<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <String as serde::Deserialize>::deserialize(deserializer)?;
    match value.as_str() {
        "keep" | "when_idle" => Ok(value),
        _ => Err(serde::de::Error::custom(format!(
            "unsupported cleanup policy '{value}'; expected 'keep' or 'when_idle'"
        ))),
    }
}

fn deserialize_delete_branch<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = <String as serde::Deserialize>::deserialize(deserializer)?;
    match value.as_str() {
        "never" | "when_removed" => Ok(value),
        _ => Err(serde::de::Error::custom(format!(
            "unsupported delete_branch policy '{value}'; expected 'never' or 'when_removed'"
        ))),
    }
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
    /// Deprecated compatibility key. Multiple active tasks per agent are
    /// supported, and this value has no effect on `task claim`, `task start`,
    /// or `task assign` (design 2026-08-21 §11, invariant I19/I21; DEC-0004).
    /// It remains accepted so existing configuration files keep parsing;
    /// capacity policy belongs to the commander or the external harness.
    /// Do not add enforcement behind this key (CTX-0043).
    #[serde(default = "default_true")]
    pub single_active_task_per_agent: bool,
    #[serde(default)]
    pub strict_completion: bool,
    /// Cap for task listings (`task list`); guards against unbounded scans
    /// over large projects. Override with `task list --limit`.
    #[serde(default = "default_task_list_limit")]
    pub list_limit: u64,
    /// Opt-in enforcement policies (Issue #213; alias for [policy]).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enforcement: Option<PolicyConfig>,
    /// Optional pre-completion verification hook configuration (CTX-0010, Issue #223).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<TaskCompletionConfig>,
}

/// Pre-completion verification hook settings under `[task.completion]` (CTX-0010, Issue #223).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub struct TaskCompletionConfig {
    /// Enforce pre-completion verification commands during `task complete`.
    #[serde(default)]
    pub strict: bool,
    /// Named verification profile to run during `task complete` (e.g. "default", "fast", "strict", "release").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    /// Explicit local commands to execute for `carryctx.pre-task-complete`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
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
            enforcement: None,
            completion: None,
        }
    }
}

/// Strict enforcement policies for task lifecycle invariants (Issue #213).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub struct PolicyConfig {
    /// Rejects `handoff accept` or `task claim` if the acting agent lacks the declared `required_role`.
    #[serde(default)]
    pub enforce_role_matching: bool,

    /// Fails `task start` or `task claim` if the acting agent already has a task in `in_progress`.
    #[serde(default)]
    pub single_active_task: bool,
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

/// A verification profile entry under `[verification.profiles.<name>]` (CTX-0010, Issue #223).
///
/// Accepts either a direct list of command strings (`fast = ["cargo check"]`)
/// or a nested table (`[verification.profiles.fast]\ncommands = ["cargo check"]`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(untagged)]
pub enum VerificationProfile {
    Commands(Vec<String>),
    Table {
        #[serde(default)]
        commands: Vec<String>,
    },
}

impl VerificationProfile {
    pub fn commands(&self) -> &[String] {
        match self {
            Self::Commands(cmds) => cmds,
            Self::Table { commands } => commands,
        }
    }
}

impl Default for VerificationProfile {
    fn default() -> Self {
        Self::Commands(Vec::new())
    }
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default, PartialEq, Eq)]
pub struct VerificationConfig {
    #[serde(default)]
    pub commands: Vec<String>,
    /// Enforce verification commands during `task complete` (`carryctx.pre-task-complete`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub strict: bool,
    /// Alias for `strict`: whether pre-completion verification runs automatically on `task complete`.
    #[serde(default, skip_serializing_if = "is_false")]
    pub enforce_on_complete: bool,
    /// Default verification profile name (e.g. "default", "fast", "strict", "release").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_profile: Option<String>,
    /// Named verification profiles mapping profile names to local command lists.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub profiles: std::collections::BTreeMap<String, VerificationProfile>,
}

/// Global-only security settings.
///
/// `allow_project_commands` is the first of the two trust keys: even a
/// trusted project cannot execute repository-provided policy while this is
/// `false`. Defaults to `false` (deny by default). This table is only ever
/// honored from the global config file; the CLI loader discards any
/// project-level `[security]` table.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, Default)]
pub struct SecurityConfig {
    #[serde(default)]
    pub allow_project_commands: bool,
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

    #[test]
    fn unsupported_cleanup_policies_are_rejected() {
        for key in ["on_task_completed", "on_task_cancelled", "delete_branch"] {
            let value = if key == "delete_branch" {
                "when_idle"
            } else {
                "always"
            };
            let raw = format!("[worktree.cleanup]\n{key} = \"{value}\"\n");
            assert!(toml::from_str::<CarryCtxConfig>(&raw).is_err(), "{key}");
        }
    }

    #[test]
    fn policy_defaults_are_false() {
        let config = CarryCtxConfig::default();
        assert!(!config.policy.enforce_role_matching);
        assert!(!config.policy.single_active_task);
        assert_eq!(config.effective_policy(), PolicyConfig::default());
    }

    #[test]
    fn policy_parses_toml_and_effective_policy_resolves() {
        let raw = r#"
[policy]
enforce_role_matching = true
single_active_task = true
"#;
        let config: CarryCtxConfig = toml::from_str(raw).unwrap();
        assert!(config.policy.enforce_role_matching);
        assert!(config.policy.single_active_task);
        assert!(config.effective_policy().enforce_role_matching);
        assert!(config.effective_policy().single_active_task);

        let raw_alias = r#"
[task.enforcement]
enforce_role_matching = true
single_active_task = false
"#;
        let config_alias: CarryCtxConfig = toml::from_str(raw_alias).unwrap();
        assert!(config_alias.effective_policy().enforce_role_matching);
        assert!(!config_alias.effective_policy().single_active_task);
    }

    #[test]
    fn verification_profiles_and_task_completion_resolve() {
        let raw = r#"
[task.completion]
strict = true
profile = "fast"

[verification.profiles]
default = ["cargo clippy", "cargo test"]
fast = ["cargo check"]

[verification.profiles.release]
commands = ["cargo test --release"]
"#;
        let config: CarryCtxConfig = toml::from_str(raw).unwrap();
        let (profile, cmds) = config.resolve_pre_task_complete(None).unwrap().unwrap();
        assert_eq!(profile, "fast");
        assert_eq!(cmds, vec!["cargo check"]);

        let (explicit_prof, release_cmds) = config
            .resolve_pre_task_complete(Some("release"))
            .unwrap()
            .unwrap();
        assert_eq!(explicit_prof, "release");
        assert_eq!(release_cmds, vec!["cargo test --release"]);

        let err = config
            .resolve_pre_task_complete(Some("missing"))
            .unwrap_err();
        assert_eq!(err.code, "CONFIGURATION_ERROR");
    }
}
