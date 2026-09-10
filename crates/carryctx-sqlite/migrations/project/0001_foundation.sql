CREATE TABLE IF NOT EXISTS schema_migrations (
  version INTEGER PRIMARY KEY CHECK (version > 0),
  name TEXT NOT NULL CHECK (length(trim(name)) > 0),
  checksum TEXT NOT NULL CHECK (
    length(checksum) = 64
    AND checksum NOT GLOB '*[^0-9a-f]*'
  ),
  applied_at TEXT NOT NULL CHECK (length(trim(applied_at)) > 0)
);

CREATE TABLE projects (
  id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
  name TEXT NOT NULL CHECK (length(trim(name)) > 0),
  task_prefix TEXT NOT NULL CHECK (length(trim(task_prefix)) > 0),
  repository_root TEXT NOT NULL UNIQUE CHECK (length(trim(repository_root)) > 0),
  git_common_dir TEXT NOT NULL UNIQUE CHECK (length(trim(git_common_dir)) > 0),
  main_branch TEXT NOT NULL CHECK (length(trim(main_branch)) > 0),
  schema_version INTEGER NOT NULL CHECK (schema_version > 0),
  created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
  updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0)
);

CREATE TABLE operations (
  id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
  kind TEXT NOT NULL CHECK (length(trim(kind)) > 0),
  state TEXT NOT NULL CHECK (state IN ('prepared', 'completed', 'failed')),
  payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
  failure_code TEXT,
  created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
  updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
  CHECK (
    (state = 'failed' AND failure_code IS NOT NULL AND length(trim(failure_code)) > 0)
    OR (state IN ('prepared', 'completed') AND failure_code IS NULL)
  )
);

CREATE TABLE events (
  id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
  type TEXT NOT NULL CHECK (length(trim(type)) > 0),
  aggregate_type TEXT NOT NULL CHECK (length(trim(aggregate_type)) > 0),
  aggregate_id TEXT NOT NULL CHECK (length(trim(aggregate_id)) > 0),
  payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
  occurred_at TEXT NOT NULL CHECK (length(trim(occurred_at)) > 0)
);

CREATE INDEX idx_operations_state_updated
  ON operations(state, updated_at DESC);
CREATE INDEX idx_events_occurred_at
  ON events(occurred_at DESC);
CREATE INDEX idx_events_type_occurred_at
  ON events(type, occurred_at DESC);
CREATE INDEX idx_events_project_occurred_at
  ON events(project_id, occurred_at DESC);

CREATE TRIGGER events_reject_update
BEFORE UPDATE ON events
BEGIN
  SELECT RAISE(ABORT, 'events are append-only');
END;

CREATE TRIGGER events_reject_delete
BEFORE DELETE ON events
BEGIN
  SELECT RAISE(ABORT, 'events are append-only');
END;
