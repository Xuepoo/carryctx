use crate::domain::graph::{GraphEdge, GraphNode};
use crate::error::CarryCtxError;
use rusqlite::{Connection, OptionalExtension, params};

pub struct GraphRepository<'a> {
    pub conn: &'a Connection,
}

impl<'a> GraphRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    pub fn insert_node(&self, node: &GraphNode) -> Result<(), CarryCtxError> {
        let meta_str = serde_json::to_string(&node.metadata).map_err(|e| {
            CarryCtxError::database_error(format!("Failed to serialize metadata: {e}"))
        })?;

        self.conn.execute(
            "INSERT INTO graph_nodes (id, node_type, name, description, metadata, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                node.id,
                node.node_type,
                node.name,
                node.description,
                meta_str,
                node.created_at,
                node.updated_at
            ],
        ).map_err(|e| CarryCtxError::database_error(format!("Failed to insert graph node: {e}")))?;
        Ok(())
    }

    pub fn get_node(&self, id: &str) -> Result<Option<GraphNode>, CarryCtxError> {
        let mut stmt = self.conn.prepare("SELECT id, node_type, name, description, metadata, created_at, updated_at FROM graph_nodes WHERE id = ?1")
            .map_err(|e| CarryCtxError::database_error(e.to_string()))?;

        let node = stmt
            .query_row(params![id], |row| {
                let meta_str: String = row.get(4)?;
                let metadata = serde_json::from_str(&meta_str).unwrap_or(serde_json::Value::Null);

                Ok(GraphNode {
                    id: row.get(0)?,
                    node_type: row.get(1)?,
                    name: row.get(2)?,
                    description: row.get(3)?,
                    metadata,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })
            .optional()
            .map_err(|e| CarryCtxError::database_error(e.to_string()))?;

        Ok(node)
    }

    pub fn insert_edge(&self, edge: &GraphEdge) -> Result<(), CarryCtxError> {
        let meta_str = serde_json::to_string(&edge.metadata).map_err(|e| {
            CarryCtxError::database_error(format!("Failed to serialize metadata: {e}"))
        })?;

        self.conn.execute(
            "INSERT INTO graph_edges (source_id, target_id, relation_type, created_at, created_by, metadata)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                edge.source_id,
                edge.target_id,
                edge.relation_type,
                edge.created_at,
                edge.created_by,
                meta_str
            ],
        ).map_err(|e| CarryCtxError::database_error(format!("Failed to insert graph edge: {e}")))?;
        Ok(())
    }

    pub fn get_edges_for_node(&self, id: &str) -> Result<Vec<GraphEdge>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT source_id, target_id, relation_type, created_at, created_by, metadata 
             FROM graph_edges WHERE source_id = ?1 OR target_id = ?1",
            )
            .map_err(|e| CarryCtxError::database_error(e.to_string()))?;

        let edges = stmt
            .query_map(params![id], |row| {
                let meta_str: String = row.get(5)?;
                let metadata = serde_json::from_str(&meta_str).unwrap_or(serde_json::Value::Null);

                Ok(GraphEdge {
                    source_id: row.get(0)?,
                    target_id: row.get(1)?,
                    relation_type: row.get(2)?,
                    created_at: row.get(3)?,
                    created_by: row.get(4)?,
                    metadata,
                })
            })
            .map_err(|e| CarryCtxError::database_error(e.to_string()))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| CarryCtxError::database_error(e.to_string()))?;

        Ok(edges)
    }

    pub fn get_node_by_name_and_type(
        &self,
        name: &str,
        node_type: &str,
    ) -> Result<Option<GraphNode>, CarryCtxError> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT id, node_type, name, description, metadata, created_at, updated_at 
             FROM graph_nodes WHERE name = ?1 AND node_type = ?2 LIMIT 1",
            )
            .map_err(|e| CarryCtxError::database_error(e.to_string()))?;

        let node = stmt
            .query_row(params![name, node_type], |row| {
                let meta_str: String = row.get(4)?;
                let metadata = serde_json::from_str(&meta_str).unwrap_or(serde_json::Value::Null);
                Ok(GraphNode {
                    id: row.get(0)?,
                    node_type: row.get(1)?,
                    name: row.get(2)?,
                    description: row.get(3)?,
                    metadata,
                    created_at: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            })
            .optional()
            .map_err(|e| CarryCtxError::database_error(e.to_string()))?;
        Ok(node)
    }

    pub fn get_edge(
        &self,
        source_id: &str,
        target_id: &str,
        relation_type: &str,
    ) -> Result<Option<GraphEdge>, CarryCtxError> {
        let mut stmt = self.conn.prepare(
            "SELECT source_id, target_id, relation_type, created_at, created_by, metadata 
             FROM graph_edges WHERE source_id = ?1 AND target_id = ?2 AND relation_type = ?3 LIMIT 1"
        ).map_err(|e| CarryCtxError::database_error(e.to_string()))?;

        let edge = stmt
            .query_row(params![source_id, target_id, relation_type], |row| {
                let meta_str: String = row.get(5)?;
                let metadata = serde_json::from_str(&meta_str).unwrap_or(serde_json::Value::Null);
                Ok(GraphEdge {
                    source_id: row.get(0)?,
                    target_id: row.get(1)?,
                    relation_type: row.get(2)?,
                    created_at: row.get(3)?,
                    created_by: row.get(4)?,
                    metadata,
                })
            })
            .optional()
            .map_err(|e| CarryCtxError::database_error(e.to_string()))?;
        Ok(edge)
    }
}
