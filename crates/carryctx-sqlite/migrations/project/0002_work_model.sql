CREATE TABLE IF NOT EXISTS sequences (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  kind TEXT NOT NULL CHECK (length(trim(kind)) > 0),
  next_value INTEGER NOT NULL DEFAULT 1 CHECK (next_value >= 1),
  PRIMARY KEY (project_id, kind)
);

CREATE TABLE IF NOT EXISTS agents (
  id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL CHECK (length(trim(name)) > 0),
  provider TEXT NOT NULL DEFAULT '' CHECK (length(trim(provider)) > 0),
  role TEXT,
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive', 'deactivated')),
  metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
  created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
  updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
  last_active_at TEXT
);

CREATE TABLE IF NOT EXISTS tasks (
  id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  display_id TEXT NOT NULL CHECK (length(trim(display_id)) > 0),
  title TEXT NOT NULL CHECK (length(trim(title)) > 0),
  description TEXT,
  status TEXT NOT NULL DEFAULT 'planned' CHECK (
    status IN (
      'planned',
      'ready',
      'in_progress',
      'blocked',
      'review',
      'completed',
      'cancelled'
    )
  ),
  priority TEXT NOT NULL DEFAULT 'normal' CHECK (
    priority IN ('low', 'normal', 'high', 'urgent')
  ),
  owner_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
  parent_task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
  metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
  created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
  updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
  started_at TEXT,
  completed_at TEXT
);

CREATE TABLE IF NOT EXISTS task_dependencies (
  id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  prerequisite_task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
  kind TEXT NOT NULL DEFAULT 'strong' CHECK (kind IN ('strong', 'informational')),
  created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
  CHECK (prerequisite_task_id != task_id)
);

ALTER TABLE events ADD COLUMN actor_agent_id TEXT REFERENCES agents(id);
ALTER TABLE events ADD COLUMN session_id TEXT;
ALTER TABLE events ADD COLUMN task_id TEXT REFERENCES tasks(id);

CREATE UNIQUE INDEX IF NOT EXISTS agents_project_name_uq
  ON agents(project_id, name);
CREATE UNIQUE INDEX IF NOT EXISTS tasks_project_display_id_uq
  ON tasks(project_id, display_id);
CREATE UNIQUE INDEX IF NOT EXISTS task_dependencies_edge_uq
  ON task_dependencies(task_id, prerequisite_task_id);
CREATE INDEX IF NOT EXISTS tasks_project_status_owner_idx
  ON tasks(project_id, status, owner_agent_id);
CREATE INDEX IF NOT EXISTS events_task_occurred_idx
  ON events(task_id, occurred_at DESC, id);
