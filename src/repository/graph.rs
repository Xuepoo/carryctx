use crate::domain::graph::{GraphEdge, GraphNode};
use crate::error::CarryCtxError;
use rusqlite::{Connection, OptionalExtension, params};
use std::time::Duration;

/// Bounded retry for transient SQLite busy/locked errors on read-only graph
/// queries. Connections already set `busy_timeout=10000`, but a WAL recovery
/// or checkpoint window right after a burst of concurrent writers can still
/// surface `SQLITE_BUSY`/`SQLITE_LOCKED`; graph export must answer through
/// its normal envelope instead of failing the whole invocation.
const GRAPH_READ_RETRY_MAX_ATTEMPTS: u32 = 5;
const GRAPH_READ_RETRY_DELAY_MS: u64 = 25;

fn is_transient_busy_error(err: &rusqlite::Error) -> bool {
    matches!(
        err.sqlite_error_code(),
        Some(rusqlite::ffi::ErrorCode::DatabaseBusy)
            | Some(rusqlite::ffi::ErrorCode::DatabaseLocked)
    )
}

pub struct GraphRepository<'a> {
    pub conn: &'a Connection,
}

impl<'a> GraphRepository<'a> {
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Run a read-only graph query, absorbing transient busy/locked errors
    /// with linear backoff up to [`GRAPH_READ_RETRY_MAX_ATTEMPTS`]. Any other
    /// error — or persistent busyness past the budget — maps exactly like
    /// before: `DATABASE_ERROR` with the bare rusqlite message.
    fn read_with_busy_retry<T>(
        &self,
        mut query: impl FnMut() -> Result<T, rusqlite::Error>,
    ) -> Result<T, CarryCtxError> {
        let mut attempt: u32 = 0;
        loop {
            match query() {
                Ok(value) => return Ok(value),
                Err(err) if is_transient_busy_error(&err) => {
                    attempt += 1;
                    if attempt >= GRAPH_READ_RETRY_MAX_ATTEMPTS {
                        return Err(CarryCtxError::database_error(err.to_string()));
                    }
                    std::thread::sleep(Duration::from_millis(
                        GRAPH_READ_RETRY_DELAY_MS * u64::from(attempt),
                    ));
                }
                Err(err) => return Err(CarryCtxError::database_error(err.to_string())),
            }
        }
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

    pub fn count_nodes(&self) -> Result<usize, CarryCtxError> {
        let count: i64 = self
            .conn
            .query_row("SELECT COUNT(*) FROM graph_nodes", [], |r| r.get(0))
            .map_err(|e| CarryCtxError::database_error(e.to_string()))?;
        Ok(count as usize)
    }

    pub fn list_full_graph(&self) -> Result<(Vec<GraphNode>, Vec<GraphEdge>), CarryCtxError> {
        let nodes = self.read_with_busy_retry(|| {
            let mut stmt_nodes = self
                .conn
                .prepare("SELECT id, node_type, name, description, metadata, created_at, updated_at FROM graph_nodes")?;

            let rows = stmt_nodes.query_map([], |row| {
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
            })?;
            rows.collect()
        })?;

        let edges = self.read_with_busy_retry(|| {
            let mut stmt_edges = self
                .conn
                .prepare("SELECT source_id, target_id, relation_type, created_at, created_by, metadata FROM graph_edges")?;

            let rows = stmt_edges.query_map([], |row| {
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
            })?;
            rows.collect()
        })?;

        Ok((nodes, edges))
    }

    pub fn list_graph_filtered(
        &self,
        node_type: &str,
    ) -> Result<(Vec<GraphNode>, Vec<GraphEdge>), CarryCtxError> {
        let nodes: Vec<GraphNode> = self.read_with_busy_retry(|| {
            let mut stmt_nodes = self
                .conn
                .prepare("SELECT id, node_type, name, description, metadata, created_at, updated_at FROM graph_nodes WHERE node_type = ?1")?;

            // Plain array parameter (not the `params!` macro): keeps the
            // `node_type` reference directly in the AST so static analyzers
            // see the use inside this closure.
            let rows = stmt_nodes.query_map([node_type], |row| {
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
            })?;
            rows.collect()
        })?;

        let node_ids: std::collections::HashSet<String> =
            nodes.iter().map(|n| n.id.clone()).collect();

        let (_all_nodes, all_edges) = self.list_full_graph()?;
        let filtered_edges = all_edges
            .into_iter()
            .filter(|e| node_ids.contains(&e.source_id) || node_ids.contains(&e.target_id))
            .collect();

        Ok((nodes, filtered_edges))
    }
}

#[cfg(test)]
mod busy_retry_tests {
    use super::*;
    use rusqlite::ffi;

    fn busy_error() -> rusqlite::Error {
        rusqlite::Error::SqliteFailure(
            ffi::Error::new(ffi::SQLITE_BUSY),
            Some("database is locked".into()),
        )
    }

    #[test]
    fn read_retry_absorbs_transient_busy_then_succeeds() {
        let conn = Connection::open_in_memory().unwrap();
        let repo = GraphRepository::new(&conn);
        let mut attempts = 0;
        let value = repo
            .read_with_busy_retry(|| {
                attempts += 1;
                if attempts < 3 {
                    Err(busy_error())
                } else {
                    Ok(42u32)
                }
            })
            .unwrap();
        assert_eq!(value, 42);
        assert_eq!(attempts, 3);
    }

    #[test]
    fn read_retry_gives_up_after_bounded_attempts_on_persistent_busy() {
        let conn = Connection::open_in_memory().unwrap();
        let repo = GraphRepository::new(&conn);
        let mut attempts = 0;
        let err = repo
            .read_with_busy_retry::<()>(&mut || {
                attempts += 1;
                Err(busy_error())
            })
            .unwrap_err();
        assert_eq!(err.code, "DATABASE_ERROR");
        assert_eq!(attempts, GRAPH_READ_RETRY_MAX_ATTEMPTS);
    }

    #[test]
    fn read_retry_maps_non_busy_errors_immediately_without_retrying() {
        let conn = Connection::open_in_memory().unwrap();
        let repo = GraphRepository::new(&conn);
        let mut attempts = 0;
        let err = repo
            .read_with_busy_retry::<()>(&mut || {
                attempts += 1;
                Err(rusqlite::Error::InvalidColumnName("nope".into()))
            })
            .unwrap_err();
        assert_eq!(err.code, "DATABASE_ERROR");
        assert_eq!(attempts, 1);
    }

    #[test]
    fn read_retry_also_absorbs_database_locked() {
        let conn = Connection::open_in_memory().unwrap();
        let repo = GraphRepository::new(&conn);
        let mut attempts = 0;
        let value = repo
            .read_with_busy_retry(|| {
                attempts += 1;
                if attempts < 2 {
                    Err(rusqlite::Error::SqliteFailure(
                        ffi::Error::new(ffi::SQLITE_LOCKED),
                        None,
                    ))
                } else {
                    Ok("ok")
                }
            })
            .unwrap();
        assert_eq!(value, "ok");
        assert_eq!(attempts, 2);
    }
}
