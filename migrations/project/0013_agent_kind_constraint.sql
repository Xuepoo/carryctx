CREATE TABLE agents_new (
  id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL CHECK (length(trim(name)) > 0),
  provider TEXT NOT NULL DEFAULT '' CHECK (length(trim(provider)) > 0),
  role TEXT,
  kind TEXT CHECK (kind IS NULL OR kind IN ('commander', 'subagent')),
  status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'inactive', 'deactivated')),
  metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
  created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
  updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
  last_active_at TEXT
);
INSERT INTO agents_new (id, project_id, name, provider, role, kind, status, metadata_json, created_at, updated_at, last_active_at)
SELECT id, project_id, name, provider, role, kind, status, metadata_json, created_at, updated_at, last_active_at FROM agents;
DROP TABLE agents;
ALTER TABLE agents_new RENAME TO agents;
CREATE UNIQUE INDEX IF NOT EXISTS agents_project_name_uq ON agents(project_id, name);
CREATE UNIQUE INDEX IF NOT EXISTS agents_project_id_uq ON agents(project_id, id);
