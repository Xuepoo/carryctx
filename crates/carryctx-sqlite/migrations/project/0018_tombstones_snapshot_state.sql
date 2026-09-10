-- CTX-0140 (merge milestone; design
-- `2026-09-10-mergeable-git-managed-state.md` §1.3): durable tombstones for
-- hard-deleted rows so a three-way merge can distinguish "deleted on this
-- side" from "never seen". A side table only: ordinary reads and command
-- output stay unchanged. Nothing prunes tombstone rows automatically.
--
-- row_id is the row's primary key value, or a canonical composite key
-- (JSON array) for tables without a single-column primary key
-- (team_members). Delete paths append one row per deleted row in the same
-- transaction as the delete; duplicate keys keep the earliest deleted_at
-- (merge unions tombstones by key and keeps the earliest deletion).

CREATE TABLE IF NOT EXISTS tombstones (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  table_name TEXT NOT NULL CHECK (length(trim(table_name)) > 0),
  row_id     TEXT NOT NULL CHECK (length(trim(row_id)) > 0),
  deleted_at TEXT NOT NULL CHECK (length(trim(deleted_at)) > 0),
  deleted_by TEXT,
  reason     TEXT,
  PRIMARY KEY (project_id, table_name, row_id)
);

-- CTX-0140 / design §2.1 and §3.2: local-only snapshot bookkeeping for merge
-- base resolution (`last_export_id`, `last_snapshot_commit`). Never exported
-- and never part of any ctxpack bundle.
CREATE TABLE IF NOT EXISTS snapshot_state (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  key        TEXT NOT NULL CHECK (length(trim(key)) > 0),
  value      TEXT NOT NULL,
  updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
  PRIMARY KEY (project_id, key)
);
