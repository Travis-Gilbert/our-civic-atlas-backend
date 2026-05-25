-- Phase 2: optimistic-concurrency versioning for event_placements
-- and event_tasks. Lets the planner's drag-and-edit flow use a
-- "last writer wins, surface conflicts" model:
--
--   1. Client sends UPDATE with expected_version.
--   2. Server runs `UPDATE ... WHERE id = $1 AND version = $expected`.
--   3. If the WHERE drops the row, the client raced with another
--      planner; the mutation responds with stale_write=true and
--      the client refetches.
--
-- The trigger bumps version + updated_at in a single before-update
-- step so callers never compute version themselves. New rows start
-- at version=1.
--
-- The notify trigger in 0013 fires AFTER UPDATE and reads the bumped
-- version off NEW.version directly.
--
-- Migration number bumped from spec's 0011 because 0011 is taken by
-- 0011_event_layers.sql (Phase 1). The trigger function name stays
-- generic (`bump_version`) so future tenant-scoped tables can reuse
-- it without renaming.

ALTER TABLE event_placements
    ADD COLUMN version bigint NOT NULL DEFAULT 1;

ALTER TABLE event_tasks
    ADD COLUMN version bigint NOT NULL DEFAULT 1;

CREATE OR REPLACE FUNCTION bump_version() RETURNS trigger AS $$
BEGIN
    -- Only bump when something other than the version column itself
    -- changed. The `IS DISTINCT FROM` test handles NULL safely, so
    -- nullable columns (like notes, owner_user_id) compare cleanly.
    IF OLD IS DISTINCT FROM NEW THEN
        NEW.version := OLD.version + 1;
        NEW.updated_at := now();
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER event_placements_bump_version
    BEFORE UPDATE ON event_placements
    FOR EACH ROW EXECUTE FUNCTION bump_version();

CREATE TRIGGER event_tasks_bump_version
    BEFORE UPDATE ON event_tasks
    FOR EACH ROW EXECUTE FUNCTION bump_version();
