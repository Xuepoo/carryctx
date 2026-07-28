-- Full-text search across tasks, progress items, checkpoints, and decisions
-- (see https://github.com/Xuepoo/carryctx/issues/45).
--
-- These are plain (non external-content) FTS5 tables rather than
-- `content='<table>'` external-content ones: checkpoints' searchable text
-- is derived from flattening three JSON array columns, which has no single
-- matching source column for FTS5's external-content row lookups to bind
-- to. Plain FTS5 tables duplicate the indexed text on disk, but the corpus
-- this covers is small (tens of KB per project in practice) and every
-- table behaves identically and predictably, including `snippet()`, which
-- external-content tables complicate for derived columns. Triggers keep
-- every FTS index in sync with its source table on insert/update/delete.
-- Existing rows are backfilled below so upgrading a pre-existing database
-- makes the whole corpus searchable immediately.

-- ── tasks ────────────────────────────────────────────────────────────────
CREATE VIRTUAL TABLE IF NOT EXISTS tasks_fts USING fts5(title, description);

CREATE TRIGGER IF NOT EXISTS tasks_fts_ai AFTER INSERT ON tasks BEGIN
  INSERT INTO tasks_fts(rowid, title, description)
  VALUES (new.rowid, new.title, new.description);
END;
CREATE TRIGGER IF NOT EXISTS tasks_fts_ad AFTER DELETE ON tasks BEGIN
  DELETE FROM tasks_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER IF NOT EXISTS tasks_fts_au AFTER UPDATE ON tasks BEGIN
  DELETE FROM tasks_fts WHERE rowid = old.rowid;
  INSERT INTO tasks_fts(rowid, title, description)
  VALUES (new.rowid, new.title, new.description);
END;

INSERT INTO tasks_fts(rowid, title, description)
SELECT rowid, title, description FROM tasks;

-- ── progress_items ───────────────────────────────────────────────────────
CREATE VIRTUAL TABLE IF NOT EXISTS progress_items_fts USING fts5(content);

CREATE TRIGGER IF NOT EXISTS progress_items_fts_ai AFTER INSERT ON progress_items BEGIN
  INSERT INTO progress_items_fts(rowid, content)
  VALUES (new.rowid, new.content);
END;
CREATE TRIGGER IF NOT EXISTS progress_items_fts_ad AFTER DELETE ON progress_items BEGIN
  DELETE FROM progress_items_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER IF NOT EXISTS progress_items_fts_au AFTER UPDATE ON progress_items BEGIN
  DELETE FROM progress_items_fts WHERE rowid = old.rowid;
  INSERT INTO progress_items_fts(rowid, content)
  VALUES (new.rowid, new.content);
END;

INSERT INTO progress_items_fts(rowid, content)
SELECT rowid, content FROM progress_items;

-- ── checkpoints ──────────────────────────────────────────────────────────
-- done_items_json/remaining_items_json/notes_json are JSON arrays of free
-- text (see 0005_checkpoints.sql); json_each flattens them into one
-- searchable blob per checkpoint rather than indexing the raw JSON syntax.
CREATE VIRTUAL TABLE IF NOT EXISTS checkpoints_fts USING fts5(body);

CREATE TRIGGER IF NOT EXISTS checkpoints_fts_ai AFTER INSERT ON checkpoints BEGIN
  INSERT INTO checkpoints_fts(rowid, body)
  VALUES (
    new.rowid,
    (
      SELECT group_concat(value, ' ') FROM (
        SELECT value FROM json_each(new.done_items_json)
        UNION ALL
        SELECT value FROM json_each(new.remaining_items_json)
        UNION ALL
        SELECT value FROM json_each(new.notes_json)
      )
    )
  );
END;
CREATE TRIGGER IF NOT EXISTS checkpoints_fts_ad AFTER DELETE ON checkpoints BEGIN
  DELETE FROM checkpoints_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER IF NOT EXISTS checkpoints_fts_au AFTER UPDATE ON checkpoints BEGIN
  DELETE FROM checkpoints_fts WHERE rowid = old.rowid;
  INSERT INTO checkpoints_fts(rowid, body)
  VALUES (
    new.rowid,
    (
      SELECT group_concat(value, ' ') FROM (
        SELECT value FROM json_each(new.done_items_json)
        UNION ALL
        SELECT value FROM json_each(new.remaining_items_json)
        UNION ALL
        SELECT value FROM json_each(new.notes_json)
      )
    )
  );
END;

INSERT INTO checkpoints_fts(rowid, body)
SELECT
  rowid,
  (
    SELECT group_concat(value, ' ') FROM (
      SELECT value FROM json_each(done_items_json)
      UNION ALL
      SELECT value FROM json_each(remaining_items_json)
      UNION ALL
      SELECT value FROM json_each(notes_json)
    )
  )
FROM checkpoints;

-- ── decisions ────────────────────────────────────────────────────────────
CREATE VIRTUAL TABLE IF NOT EXISTS decisions_fts USING fts5(
  title,
  context,
  decision_body,
  consequences
);

CREATE TRIGGER IF NOT EXISTS decisions_fts_ai AFTER INSERT ON decisions BEGIN
  INSERT INTO decisions_fts(rowid, title, context, decision_body, consequences)
  VALUES (new.rowid, new.title, new.context, new.decision_body, new.consequences);
END;
CREATE TRIGGER IF NOT EXISTS decisions_fts_ad AFTER DELETE ON decisions BEGIN
  DELETE FROM decisions_fts WHERE rowid = old.rowid;
END;
CREATE TRIGGER IF NOT EXISTS decisions_fts_au AFTER UPDATE ON decisions BEGIN
  DELETE FROM decisions_fts WHERE rowid = old.rowid;
  INSERT INTO decisions_fts(rowid, title, context, decision_body, consequences)
  VALUES (new.rowid, new.title, new.context, new.decision_body, new.consequences);
END;

INSERT INTO decisions_fts(rowid, title, context, decision_body, consequences)
SELECT rowid, title, context, decision_body, consequences FROM decisions;
