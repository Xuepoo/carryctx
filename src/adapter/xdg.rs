use std::path::PathBuf;

#[derive(Clone)]
pub struct XdgPaths {
    pub data_home: PathBuf,
    pub config_home: PathBuf,
    pub state_home: PathBuf,
    pub cache_home: PathBuf,
}

impl XdgPaths {
    pub fn new() -> Self {
        let data_home = dirs_data_home();
        let config_home = dirs_config_home();
        let state_home = dirs_state_home();
        let cache_home = dirs_cache_home();
        Self {
            data_home,
            config_home,
            state_home,
            cache_home,
        }
    }

    pub fn registry_db(&self) -> PathBuf {
        self.data_home.join("carryctx").join("registry.sqlite")
    }

    pub fn global_config(&self) -> PathBuf {
        self.config_home.join("carryctx").join("config.toml")
    }

    pub fn project_state_dir(&self, git_common_dir: &std::path::Path) -> PathBuf {
        git_common_dir.join("carryctx")
    }

    pub fn project_db(&self, git_common_dir: &std::path::Path) -> PathBuf {
        self.project_state_dir(git_common_dir).join("state.sqlite")
    }

    pub fn admission_lock_dir(&self, git_common_dir: &std::path::Path) -> PathBuf {
        self.project_state_dir(git_common_dir)
            .join("locks")
            .join("command.lock")
    }

    pub fn backup_dir(&self, git_common_dir: &std::path::Path) -> PathBuf {
        self.project_state_dir(git_common_dir).join("backups")
    }

    pub fn journal_dir(&self, git_common_dir: &std::path::Path) -> PathBuf {
        self.project_state_dir(git_common_dir).join("journals")
    }
}

impl Default for XdgPaths {
    fn default() -> Self {
        Self::new()
    }
}

fn dirs_data_home() -> PathBuf {
    if let Ok(val) = std::env::var("XDG_DATA_HOME") {
        return PathBuf::from(val);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local").join("share")
}

fn dirs_config_home() -> PathBuf {
    if let Ok(val) = std::env::var("XDG_CONFIG_HOME") {
        return PathBuf::from(val);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".config")
}

fn dirs_state_home() -> PathBuf {
    if let Ok(val) = std::env::var("XDG_STATE_HOME") {
        return PathBuf::from(val);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".local").join("state")
}

fn dirs_cache_home() -> PathBuf {
    if let Ok(val) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(val);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    PathBuf::from(home).join(".cache")
}
