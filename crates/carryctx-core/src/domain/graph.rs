use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphNode {
    pub id: String,
    pub node_type: String,
    pub name: String,
    pub description: Option<String>,
    pub metadata: serde_json::Value,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GraphEdge {
    pub source_id: String,
    pub target_id: String,
    pub relation_type: String,
    pub created_at: String,
    pub created_by: Option<String>,
    pub metadata: serde_json::Value,
}

impl GraphNode {
    pub fn new(
        id: impl Into<String>,
        node_type: impl Into<String>,
        name: impl Into<String>,
        description: Option<String>,
        metadata: serde_json::Value,
        created_at: impl Into<String>,
    ) -> Self {
        let created_at = created_at.into();
        Self {
            id: id.into(),
            node_type: node_type.into(),
            name: name.into(),
            description,
            metadata,
            updated_at: created_at.clone(),
            created_at,
        }
    }
}

impl GraphEdge {
    pub fn new(
        source_id: impl Into<String>,
        target_id: impl Into<String>,
        relation_type: impl Into<String>,
        created_at: impl Into<String>,
        created_by: Option<String>,
        metadata: serde_json::Value,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            target_id: target_id.into(),
            relation_type: relation_type.into(),
            created_at: created_at.into(),
            created_by,
            metadata,
        }
    }
}
