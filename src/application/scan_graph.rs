use crate::application::extract_deps::extract_deps_for_file;
use crate::application::runtime::InvocationContext;
use crate::error::CarryCtxError;
use crate::repository::GraphRepository;
use std::path::Path;
use std::process::Command;

/// Default file extensions supported for dependency extraction.
pub const DEFAULT_EXTENSIONS: &[&str] = &["rs", "ts", "js", "tsx", "jsx"];

pub struct ScanResult {
    pub scanned: usize,
    pub skipped: usize,
    pub nodes_created: usize,
    pub edges_created: usize,
    pub errors: Vec<ScanError>,
}

pub struct ScanError {
    pub file: String,
    pub message: String,
}

/// Scan all git-tracked files under `dir`, extract dependency edges for each
/// supported file, and persist them to the graph repository.
///
/// Uses `git ls-files` as the file discovery mechanism so we only process
/// files that are part of the repository.
pub fn scan_project(
    dir: &Path,
    extensions: &[&str],
    dry_run: bool,
    repo: &GraphRepository,
    ctx: &InvocationContext,
) -> Result<ScanResult, CarryCtxError> {
    // Discover files via git ls-files
    let output = Command::new("git")
        .args(["ls-files", "--cached", "--others", "--exclude-standard"])
        .current_dir(dir)
        .output()
        .map_err(|e| CarryCtxError::git_error(format!("Failed to run git ls-files: {e}")))?;

    if !output.status.success() {
        return Err(CarryCtxError::git_error(format!(
            "git ls-files failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let files: Vec<&str> = stdout
        .lines()
        .filter(|f| {
            let p = Path::new(f);
            let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
            extensions.contains(&ext)
        })
        .collect();

    let mut result = ScanResult {
        scanned: 0,
        skipped: 0,
        nodes_created: 0,
        edges_created: 0,
        errors: vec![],
    };

    // Count nodes before scan to compute delta
    let nodes_before = count_nodes(repo);

    for file in &files {
        result.scanned += 1;

        if dry_run {
            // In dry-run mode just check the file is readable
            if !dir.join(file).exists() {
                result.skipped += 1;
            }
            continue;
        }

        // Make path relative to CWD; strip leading "./" for consistent node names
        let file_path = dir.join(file);
        let file_str_raw = file_path.to_string_lossy();
        let file_str = file_str_raw
            .strip_prefix("./")
            .unwrap_or(&file_str_raw)
            .to_string();

        match extract_deps_for_file(&file_str, repo, ctx) {
            Ok(edges) => {
                result.edges_created += edges.len();
            }
            Err(e) => {
                result.errors.push(ScanError {
                    file: file.to_string(),
                    message: e.message.clone(),
                });
            }
        }
    }

    if !dry_run {
        let nodes_after = count_nodes(repo);
        result.nodes_created = nodes_after.saturating_sub(nodes_before);
    }

    Ok(result)
}

fn count_nodes(repo: &GraphRepository) -> usize {
    // Best-effort count; silently returns 0 on error
    repo.count_nodes().unwrap_or(0)
}
