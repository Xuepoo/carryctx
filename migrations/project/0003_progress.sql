CREATE TABLE IF NOT EXISTS progress_items (
  id TEXT PRIMARY KEY,
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  display_id TEXT NOT NULL UNIQUE,
  task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  source_session_id TEXT NULL,
  type TEXT NOT NULL CHECK (type IN ('todo', 'blocker', 'risk', 'note')),
  status TEXT NOT NULL CHECK (status IN ('open', 'completed', 'removed')),
  content TEXT NOT NULL CHECK (length(trim(content)) > 0),
  percentage INTEGER NULL CHECK (percentage IS NULL OR (percentage >= 0 AND percentage <= 100)),
  position INTEGER NOT NULL,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL,
  completed_at TEXT NULL,
  removed_at TEXT NULL
);

CREATE INDEX IF NOT EXISTS progress_task_status_position_idx
  ON progress_items(task_id, status, position, id);
