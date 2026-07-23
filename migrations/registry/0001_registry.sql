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
  repository_root TEXT NOT NULL UNIQUE CHECK (length(trim(repository_root)) > 0),
  git_common_dir TEXT NOT NULL UNIQUE CHECK (length(trim(git_common_dir)) > 0),
  config_path TEXT NOT NULL UNIQUE CHECK (length(trim(config_path)) > 0),
  last_seen_at TEXT NOT NULL CHECK (length(trim(last_seen_at)) > 0)
);

CREATE TABLE path_mappings (
  path_prefix TEXT PRIMARY KEY CHECK (length(trim(path_prefix)) > 0),
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE
);

CREATE INDEX idx_path_mappings_project
  ON path_mappings(project_id);
CREATE INDEX idx_path_mappings_longest
  ON path_mappings(length(path_prefix) DESC, path_prefix);
