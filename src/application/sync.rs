use crate::adapter::filesystem;
use crate::adapter::git::GitCli;
use crate::adapter::xdg::XdgPaths;
use crate::error::{CarryCtxError, ExitCode};
use std::path::Path;

pub fn sync_push(
    project_path: &Path,
    remote_path: &str,
) -> Result<serde_json::Value, CarryCtxError> {
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let xdg = XdgPaths::new();
    let db_path = xdg.project_db(&gp.git_common_dir);

    if !db_path.exists() {
        return Err(CarryCtxError::resource_not_found(
            "Project database not found to push.",
        ));
    }

    let remote = Path::new(remote_path);
    filesystem::ensure_dir(remote)?;

    let target_db = remote.join(format!(
        "{}.sqlite",
        gp.git_common_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    ));
    std::fs::copy(&db_path, &target_db).map_err(|e| {
        CarryCtxError::new(
            "SYNC_ERROR",
            format!("Failed to copy DB to remote: {e}"),
            ExitCode::General,
        )
    })?;

    Ok(serde_json::json!({
        "status": "pushed",
        "remote": target_db.to_string_lossy(),
        "bytes": std::fs::metadata(&target_db).map(|m| m.len()).unwrap_or(0),
    }))
}

pub fn sync_pull(
    project_path: &Path,
    remote_path: &str,
) -> Result<serde_json::Value, CarryCtxError> {
    let git = GitCli::new();
    let gp = git.discover(project_path)?;
    let xdg = XdgPaths::new();
    let db_path = xdg.project_db(&gp.git_common_dir);

    let remote = Path::new(remote_path);
    let target_db = remote.join(format!(
        "{}.sqlite",
        gp.git_common_dir
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
    ));

    if !target_db.exists() {
        return Err(CarryCtxError::resource_not_found(format!(
            "Remote database not found at {}",
            target_db.display()
        )));
    }

    filesystem::ensure_dir(db_path.parent().unwrap())?;

    std::fs::copy(&target_db, &db_path).map_err(|e| {
        CarryCtxError::new(
            "SYNC_ERROR",
            format!("Failed to copy DB from remote: {e}"),
            ExitCode::General,
        )
    })?;

    Ok(serde_json::json!({
        "status": "pulled",
        "local": db_path.to_string_lossy(),
        "bytes": std::fs::metadata(&db_path).map(|m| m.len()).unwrap_or(0),
    }))
}
