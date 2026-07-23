use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PresetManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub author: String,
    #[serde(default)]
    pub permissions: PresetPermissions,
}

#[derive(Debug, Serialize, Deserialize, Clone, Default)]
pub struct PresetPermissions {
    #[serde(default)]
    pub requires_network: bool,
    #[serde(default)]
    pub requires_filesystem: bool,
    #[serde(default)]
    pub requires_env: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PresetLockfile {
    pub version: u32,
    #[serde(default)]
    pub presets: HashMap<String, PresetLockEntry>,
}

impl Default for PresetLockfile {
    fn default() -> Self {
        Self {
            version: 1,
            presets: HashMap::new(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct PresetLockEntry {
    pub version: String,
    pub source: String,
    pub integrity: String,
    pub permissions_granted: PresetPermissions,
}
