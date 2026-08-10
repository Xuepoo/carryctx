-- Backfill ended_at for sessions that reached a terminal state before
-- `update_state` began writing it (0.5.0). Their true end is the last
-- recorded activity; without this, `stats` still has no ended_at to read and
-- falls back to last_activity_at only for sessions that remain open.
UPDATE sessions
SET ended_at = last_activity_at
WHERE ended_at IS NULL
  AND state IN ('ended', 'abandoned', 'stale');
