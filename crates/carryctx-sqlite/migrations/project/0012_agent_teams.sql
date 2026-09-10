CREATE UNIQUE INDEX IF NOT EXISTS agents_project_id_uq ON agents(project_id, id);
CREATE UNIQUE INDEX IF NOT EXISTS tasks_project_id_id_uq ON tasks(project_id, id);

CREATE TABLE IF NOT EXISTS teams (
  id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  name TEXT NOT NULL CHECK (length(trim(name)) > 0),
  commander_agent_id TEXT,
  created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
  updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
  UNIQUE (project_id, id),
  UNIQUE (project_id, name),
  UNIQUE (project_id, id, commander_agent_id),
  FOREIGN KEY (project_id, id, commander_agent_id)
    REFERENCES team_members(project_id, team_id, agent_id)
);

CREATE TABLE IF NOT EXISTS team_members (
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  team_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  role TEXT CHECK (role IS NULL OR length(trim(role)) > 0),
  created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
  updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
  PRIMARY KEY (project_id, team_id, agent_id),
  FOREIGN KEY (project_id, team_id) REFERENCES teams(project_id, id) ON DELETE CASCADE,
  FOREIGN KEY (project_id, agent_id) REFERENCES agents(project_id, id) ON DELETE CASCADE,
  UNIQUE (project_id, agent_id, team_id)
);

PRAGMA legacy_alter_table = ON;
ALTER TABLE tasks RENAME TO tasks_old;
DROP TRIGGER IF EXISTS teams_clear_task_team;
DROP TRIGGER IF EXISTS tasks_reject_cross_project_team;
DROP TRIGGER IF EXISTS tasks_reject_cross_project_team_update;

CREATE TABLE tasks_new (
  id TEXT PRIMARY KEY CHECK (length(trim(id)) > 0),
  project_id TEXT NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
  display_id TEXT NOT NULL CHECK (length(trim(display_id)) > 0),
  title TEXT NOT NULL CHECK (length(trim(title)) > 0),
  description TEXT,
  status TEXT NOT NULL DEFAULT 'planned' CHECK (status IN ('planned', 'ready', 'in_progress', 'blocked', 'review', 'completed', 'cancelled')),
  priority TEXT NOT NULL DEFAULT 'normal' CHECK (priority IN ('low', 'normal', 'high', 'urgent')),
  owner_agent_id TEXT REFERENCES agents(id) ON DELETE SET NULL,
  parent_task_id TEXT REFERENCES tasks(id) ON DELETE SET NULL,
  metadata_json TEXT NOT NULL DEFAULT '{}' CHECK (json_valid(metadata_json)),
  created_at TEXT NOT NULL CHECK (length(trim(created_at)) > 0),
  updated_at TEXT NOT NULL CHECK (length(trim(updated_at)) > 0),
  started_at TEXT,
  completed_at TEXT,
  required_role TEXT CHECK (required_role IS NULL OR length(trim(required_role)) > 0),
  team_id TEXT,
  FOREIGN KEY (project_id, team_id) REFERENCES teams(project_id, id) ON DELETE SET NULL
);

INSERT INTO tasks_new (id, project_id, display_id, title, description, status, priority,
  owner_agent_id, parent_task_id, metadata_json, created_at, updated_at,
  started_at, completed_at, required_role, team_id)
SELECT id, project_id, display_id, title, description, status, priority,
  owner_agent_id, parent_task_id, metadata_json, created_at, updated_at,
  started_at, completed_at, required_role,
  CASE WHEN old.team_id IS NOT NULL AND EXISTS (
    SELECT 1 FROM teams WHERE teams.project_id = old.project_id AND teams.id = old.team_id
  ) THEN old.team_id ELSE NULL END
FROM tasks_old AS old;

DROP TABLE tasks_old;
ALTER TABLE tasks_new RENAME TO tasks;

CREATE UNIQUE INDEX IF NOT EXISTS agents_project_name_uq ON agents(project_id, name);
CREATE UNIQUE INDEX IF NOT EXISTS tasks_project_display_id_uq ON tasks(project_id, display_id);
CREATE INDEX IF NOT EXISTS tasks_project_status_owner_idx ON tasks(project_id, status, owner_agent_id);
CREATE UNIQUE INDEX IF NOT EXISTS tasks_project_id_id_uq ON tasks(project_id, id);
CREATE INDEX IF NOT EXISTS teams_project_updated_idx ON teams(project_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS team_members_team_updated_idx ON team_members(project_id, team_id, updated_at DESC);
CREATE INDEX IF NOT EXISTS tasks_project_team_status_idx ON tasks(project_id, team_id, status);

CREATE TRIGGER IF NOT EXISTS tasks_fts_ai AFTER INSERT ON tasks BEGIN
  INSERT INTO tasks_fts(rowid, title, description) VALUES (new.rowid, new.title, new.description);
END;
CREATE TRIGGER IF NOT EXISTS tasks_fts_ad AFTER DELETE ON tasks BEGIN
  DELETE FROM tasks_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER IF NOT EXISTS tasks_fts_au AFTER UPDATE ON tasks BEGIN
  DELETE FROM tasks_fts WHERE rowid = old.rowid;
  INSERT INTO tasks_fts(rowid, title, description) VALUES (new.rowid, new.title, new.description);
END;
DELETE FROM tasks_fts;
INSERT INTO tasks_fts(rowid, title, description) SELECT rowid, title, description FROM tasks;
PRAGMA legacy_alter_table = OFF;

CREATE TRIGGER IF NOT EXISTS teams_clear_task_team
BEFORE DELETE ON teams
WHEN EXISTS (SELECT 1 FROM tasks WHERE tasks.project_id = OLD.project_id AND tasks.team_id = OLD.id)
BEGIN
  UPDATE tasks SET team_id = NULL WHERE project_id = OLD.project_id AND team_id = OLD.id;
END;
CREATE TRIGGER IF NOT EXISTS team_members_reject_commander_removal
BEFORE DELETE ON team_members
WHEN EXISTS (SELECT 1 FROM teams WHERE teams.project_id = OLD.project_id AND teams.id = OLD.team_id AND teams.commander_agent_id = OLD.agent_id)
BEGIN
  SELECT RAISE(ABORT, 'cannot remove current team commander');
END;
CREATE TRIGGER IF NOT EXISTS team_members_reject_commander_update
BEFORE UPDATE OF project_id, team_id, agent_id ON team_members
WHEN EXISTS (SELECT 1 FROM teams WHERE teams.project_id = OLD.project_id AND teams.id = OLD.team_id AND teams.commander_agent_id = OLD.agent_id)
BEGIN
  SELECT RAISE(ABORT, 'cannot move current team commander');
END;
CREATE TRIGGER IF NOT EXISTS tasks_reject_cross_project_team
BEFORE INSERT ON tasks
WHEN NEW.team_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM teams WHERE teams.project_id = NEW.project_id AND teams.id = NEW.team_id)
BEGIN
  SELECT RAISE(ABORT, 'task team must belong to task project');
END;
CREATE TRIGGER IF NOT EXISTS tasks_reject_cross_project_team_update
BEFORE UPDATE OF project_id, team_id ON tasks
WHEN NEW.team_id IS NOT NULL AND NOT EXISTS (SELECT 1 FROM teams WHERE teams.project_id = NEW.project_id AND teams.id = NEW.team_id)
BEGIN
  SELECT RAISE(ABORT, 'task team must belong to task project');
END;
