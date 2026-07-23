use std::path::Path;

use crate::adapter::config::ConfigLoader;
use crate::adapter::filesystem;
use crate::adapter::git::GitCli;
use crate::adapter::sqlite::ProjectDatabase;
use crate::adapter::sqlite_repos::SqliteSessionRepository;
use crate::adapter::unit_of_work::UnitOfWork;
use crate::adapter::xdg::XdgPaths;
use crate::domain::session::SessionState;
use crate::error::CarryCtxError;
use crate::repository::session::SessionRepository;

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Diagnosis {
    pub git: GitHealth,
    pub database: DatabaseHealth,
    pub config: ConfigHealth,
    pub sessions: SessionsHealth,
    pub locks: LocksHealth,
    pub journals: JournalsHealth,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct GitHealth {
    pub ok: bool,
    pub is_git_repo: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct DatabaseHealth {
    pub ok: bool,
    pub db_exists: bool,
    pub schema_version: Option<i64>,
    pub up_to_date: Option<bool>,
    pub integrity_check: Option<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfigHealth {
    pub ok: bool,
    pub config_exists: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct SessionsHealth {
    pub ok: bool,
    pub total: u64,
    pub active: u64,
    pub stale: u64,
    pub ended: u64,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LocksHealth {
    pub ok: bool,
    pub held: bool,
    pub stale: bool,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct JournalsHealth {
    pub ok: bool,
    pub broken: Vec<String>,
    pub errors: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct RepairResult {
    pub actions_taken: Vec<String>,
    pub errors: Vec<String>,
}

pub fn run_diagnosis(project_path: &Path, uow: &UnitOfWork) -> Result<Diagnosis, CarryCtxError> {
    let xdg = XdgPaths::new();
    let git = GitCli::new();

    let git_health = diagnose_git(project_path, &git);
    let db_path = match git.discover(project_path) {
        Ok(ref gp) => xdg.project_db(&gp.git_common_dir),
        Err(_) => return Err(CarryCtxError::git_error("Not a Git repository")),
    };
    let database_health = diagnose_database(&db_path);
    let config_health = diagnose_config(project_path, &xdg);
    let sessions_health = diagnose_sessions(uow);
    let locks_health = diagnose_locks(&xdg, &db_path);
    let journals_health = diagnose_journals(&xdg, &db_path);

    Ok(Diagnosis {
        git: git_health,
        database: database_health,
        config: config_health,
        sessions: sessions_health?,
        locks: locks_health,
        journals: journals_health,
    })
}

fn diagnose_git(project_path: &Path, git: &GitCli) -> GitHealth {
    match git.discover(project_path) {
        Ok(_) => GitHealth {
            ok: true,
            is_git_repo: true,
            errors: vec![],
        },
        Err(e) => GitHealth {
            ok: false,
            is_git_repo: false,
            errors: vec![e.to_string()],
        },
    }
}

fn diagnose_database(db_path: &Path) -> DatabaseHealth {
    if !db_path.exists() {
        return DatabaseHealth {
            ok: false,
            db_exists: false,
            schema_version: None,
            up_to_date: None,
            integrity_check: None,
            errors: vec!["Database file does not exist.".into()],
        };
    }

    let db = match ProjectDatabase::open_readonly(db_path) {
        Ok(d) => d,
        Err(e) => {
            return DatabaseHealth {
                ok: false,
                db_exists: true,
                schema_version: None,
                up_to_date: None,
                integrity_check: None,
                errors: vec![e.to_string()],
            };
        }
    };

    let integrity = db
        .connection()
        .query_row("PRAGMA integrity_check", [], |row| row.get::<_, String>(0))
        .unwrap_or_else(|_| "error".into());

    let schema_version = db.applied_version().ok();
    let up_to_date = db.is_up_to_date().ok();

    let mut errors = Vec::new();
    if integrity != "ok" {
        errors.push(format!("Integrity check: {integrity}"));
    }

    DatabaseHealth {
        ok: integrity == "ok" && up_to_date.unwrap_or(false),
        db_exists: true,
        schema_version,
        up_to_date,
        integrity_check: Some(integrity),
        errors,
    }
}

fn diagnose_config(project_path: &Path, xdg: &XdgPaths) -> ConfigHealth {
    let project_config = project_path.join(".carryctx").join("config.toml");

    let mut errors = Vec::new();
    let config_exists = project_config.exists();

    let loader = ConfigLoader::new(xdg.clone());
    if let Err(e) = loader.load(Some(project_path)) {
        errors.push(e.to_string());
    }

    ConfigHealth {
        ok: errors.is_empty(),
        config_exists,
        errors,
    }
}

fn diagnose_sessions(uow: &UnitOfWork) -> Result<SessionsHealth, CarryCtxError> {
    let conn = uow.connection();
    let repo = SqliteSessionRepository::new(conn);

    let sessions = repo.list("")?;
    let total = sessions.len() as u64;
    let active = sessions
        .iter()
        .filter(|s| s.state == SessionState::Active)
        .count() as u64;
    let stale = sessions
        .iter()
        .filter(|s| s.state == SessionState::Stale)
        .count() as u64;
    let ended = sessions
        .iter()
        .filter(|s| s.state == SessionState::Ended)
        .count() as u64;

    Ok(SessionsHealth {
        ok: true,
        total,
        active,
        stale,
        ended,
        errors: vec![],
    })
}

fn diagnose_locks(xdg: &XdgPaths, db_path: &Path) -> LocksHealth {
    let git_common_dir = db_path.parent().and_then(|p| p.parent());
    let lock_dir = match git_common_dir {
        Some(dir) => xdg.admission_lock_dir(dir),
        None => {
            return LocksHealth {
                ok: true,
                held: false,
                stale: false,
                errors: vec![],
            };
        }
    };

    if !lock_dir.exists() {
        return LocksHealth {
            ok: true,
            held: false,
            stale: false,
            errors: vec![],
        };
    }

    let meta_path = lock_dir.join("meta.json");
    let mut stale = false;
    if meta_path.exists() {
        if let Ok(meta_str) = std::fs::read_to_string(&meta_path) {
            if let Ok(meta) = serde_json::from_str::<serde_json::Value>(&meta_str) {
                if let Some(pid) = meta["pid"].as_u64() {
                    if !Path::new(&format!("/proc/{pid}")).exists() {
                        stale = true;
                    }
                }
            }
        }
    }

    LocksHealth {
        ok: !stale,
        held: true,
        stale,
        errors: if stale {
            vec!["Stale admission lock detected.".into()]
        } else {
            vec![]
        },
    }
}

fn diagnose_journals(xdg: &XdgPaths, db_path: &Path) -> JournalsHealth {
    let git_common_dir = match db_path.parent().and_then(|p| p.parent()) {
        Some(d) => d,
        None => {
            return JournalsHealth {
                ok: true,
                broken: vec![],
                errors: vec![],
            };
        }
    };

    let journal_dir = xdg.journal_dir(git_common_dir);
    let entries = match filesystem::list_journals(&journal_dir) {
        Ok(e) => e,
        Err(_) => {
            return JournalsHealth {
                ok: true,
                broken: vec![],
                errors: vec![],
            };
        }
    };

    let mut broken = Vec::new();
    for entry in &entries {
        if entry.status == "running" {
            let since_creation = chrono::Utc::now()
                .signed_duration_since(
                    chrono::DateTime::parse_from_rfc3339(&entry.created_at)
                        .unwrap_or(chrono::Utc::now().into()),
                )
                .num_hours();
            if since_creation > 24 {
                broken.push(entry.operation_id.clone());
            }
        }
    }

    let broken_count = broken.len();
    JournalsHealth {
        ok: broken_count == 0,
        broken,
        errors: if broken_count > 0 {
            vec![format!("{broken_count} broken journal entries found.")]
        } else {
            vec![]
        },
    }
}

pub fn run_repair(project_path: &Path, uow: &UnitOfWork) -> Result<RepairResult, CarryCtxError> {
    let mut actions_taken = Vec::new();
    let mut errors = Vec::new();

    let diagnosis = match run_diagnosis(project_path, uow) {
        Ok(d) => d,
        Err(e) => {
            errors.push(format!("Diagnosis failed: {e}"));
            return Ok(RepairResult {
                actions_taken,
                errors,
            });
        }
    };

    // Fix stale locks
    if diagnosis.locks.stale {
        match repair_stale_lock(project_path) {
            Ok(()) => actions_taken.push("Removed stale admission lock.".into()),
            Err(e) => errors.push(format!("Failed to remove stale lock: {e}")),
        }
    }

    // Fix stale sessions
    if diagnosis.sessions.stale > 0 {
        match repair_stale_sessions(uow) {
            Ok(count) => actions_taken.push(format!("Ended {count} stale session(s).")),
            Err(e) => errors.push(format!("Failed to end stale sessions: {e}")),
        }
    }

    // Fix broken journals
    if !diagnosis.journals.broken.is_empty() {
        match repair_broken_journals(project_path) {
            Ok(count) => actions_taken.push(format!("Cleaned {count} broken journal entry(s).")),
            Err(e) => errors.push(format!("Failed to clean journals: {e}")),
        }
    }

    Ok(RepairResult {
        actions_taken,
        errors,
    })
}

fn repair_stale_lock(project_path: &Path) -> Result<(), CarryCtxError> {
    let xdg = XdgPaths::new();
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let lock_dir = xdg.admission_lock_dir(&gp.git_common_dir);
    filesystem::release_lock(&lock_dir)
}

fn repair_stale_sessions(uow: &UnitOfWork) -> Result<u64, CarryCtxError> {
    let now = now();
    let conn = uow.connection();
    let repo = SqliteSessionRepository::new(conn);

    let stale_before = chrono::Utc::now()
        .checked_sub_signed(chrono::Duration::hours(2))
        .map(|t| t.to_rfc3339())
        .unwrap_or_else(|| now.clone());

    repo.mark_overdue_stale("", &stale_before, &now).map(|c| c)
}

fn repair_broken_journals(project_path: &Path) -> Result<usize, CarryCtxError> {
    let xdg = XdgPaths::new();
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let journal_dir = xdg.journal_dir(&gp.git_common_dir);
    let entries = filesystem::list_journals(&journal_dir)?;

    let mut cleaned = 0;
    for entry in &entries {
        if entry.status == "running" {
            let since_creation = chrono::Utc::now()
                .signed_duration_since(
                    chrono::DateTime::parse_from_rfc3339(&entry.created_at)
                        .unwrap_or(chrono::Utc::now().into()),
                )
                .num_hours();
            if since_creation > 24 {
                filesystem::remove_journal(&journal_dir, &entry.operation_id)?;
                cleaned += 1;
            }
        }
    }
    Ok(cleaned)
}
