use std::path::PathBuf;

use crate::adapter::xdg::XdgPaths;
use crate::error::CarryCtxError;

const BUNDLED_SKILLS: &[(&str, &[u8])] = &[];

#[derive(Debug, Clone, serde::Serialize)]
pub struct InstalledSkill {
    pub name: String,
    pub path: String,
    pub kind: String,
}

fn skills_dir() -> PathBuf {
    let xdg = XdgPaths::new();
    xdg.data_home.join("carryctx").join("skills")
}

pub fn install_skill(name: Option<&str>) -> Result<Vec<InstalledSkill>, CarryCtxError> {
    let skill_dir = skills_dir();
    crate::adapter::filesystem::ensure_dir(&skill_dir)?;

    let mut installed = Vec::new();

    for (skill_name, data) in BUNDLED_SKILLS {
        if let Some(filter) = name {
            if skill_name != &filter {
                continue;
            }
        }

        let target = skill_dir.join(skill_name);
        crate::adapter::filesystem::ensure_dir(&target)?;

        let file_path = target.join("SKILL.md");
        if !file_path.exists() {
            crate::adapter::filesystem::write_atomic(&file_path, data)?;
        }

        installed.push(InstalledSkill {
            name: skill_name.to_string(),
            path: target.to_string_lossy().to_string(),
            kind: "bundled".into(),
        });
    }

    if installed.is_empty() {
        if let Some(filter) = name {
            return Err(CarryCtxError::resource_not_found(format!(
                "Skill '{filter}' not found in bundled skills."
            )));
        }
    }

    Ok(installed)
}

pub fn list_skills() -> Result<Vec<InstalledSkill>, CarryCtxError> {
    let skill_dir = skills_dir();
    let mut skills = Vec::new();

    if !skill_dir.exists() {
        return Ok(skills);
    }

    let mut dir = std::fs::read_dir(&skill_dir)
        .map_err(|e| CarryCtxError::database_error(format!("Failed to read skills dir: {e}")))?;

    while let Some(Ok(entry)) = dir.next() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                skills.push(InstalledSkill {
                    name: name.to_string(),
                    path: path.to_string_lossy().to_string(),
                    kind: classify_skill(&path),
                });
            }
        }
    }

    skills.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(skills)
}

fn classify_skill(path: &std::path::Path) -> String {
    let skill_md = path.join("SKILL.md");
    if skill_md.exists() {
        "complete".into()
    } else {
        "incomplete".into()
    }
}

pub fn get_skill_path(name: &str) -> Result<String, CarryCtxError> {
    let skill_dir = skills_dir().join(name);
    if !skill_dir.exists() {
        return Err(CarryCtxError::resource_not_found(format!(
            "Skill '{name}' is not installed."
        )));
    }
    Ok(skill_dir.to_string_lossy().to_string())
}
