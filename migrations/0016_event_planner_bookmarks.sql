-- Phase 3: camera bookmarks for the planner.
--
-- One row per saved view: name + center lng/lat + zoom + pitch +
-- bearing. Useful in 3D mode especially, where the six-degrees-of-
-- freedom camera state takes effort to reconstruct from memory.
--
-- Per-user ownership: each planner has their own bookmark list
-- (referenced via event_planner_users.id from Phase 2). Phase 4 can
-- add a shared/tenant-wide bookmark scope by flipping `created_by`
-- nullable and adding a `shared` boolean.
--
-- Migration number bumped from spec's implicit position because
-- 0011..0014 are taken by Phase 1 + Phase 2, and 0015 is taken by
-- the notes table above.

CREATE TABLE event_planner_bookmarks (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    event_layer_id uuid NOT NULL REFERENCES event_layers(id) ON DELETE CASCADE,
    name text NOT NULL CHECK (length(trim(name)) > 0),
    -- Camera state. lat/lng in WGS84; zoom matches MapLibre's scale
    -- range (0..22). pitch/bearing in degrees. Sanity-checked here
    -- so a malformed mutation can't poison the bookmark list.
    center_lng double precision NOT NULL CHECK (center_lng BETWEEN -180 AND 180),
    center_lat double precision NOT NULL CHECK (center_lat BETWEEN -90 AND 90),
    zoom double precision NOT NULL CHECK (zoom BETWEEN 0 AND 22),
    pitch double precision NOT NULL DEFAULT 0 CHECK (pitch BETWEEN 0 AND 85),
    bearing double precision NOT NULL DEFAULT 0 CHECK (bearing BETWEEN -360 AND 360),
    created_by uuid REFERENCES event_planner_users(id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    -- A single planner can't have two bookmarks with the same name
    -- on the same event layer. Shared bookmarks (Phase 4) would
    -- relax this to "no duplicate name per (event_layer_id, scope)".
    UNIQUE (event_layer_id, created_by, name)
);

CREATE INDEX idx_event_planner_bookmarks_event
    ON event_planner_bookmarks (tenant_id, event_layer_id, created_at DESC);

ALTER TABLE event_planner_bookmarks ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation_event_planner_bookmarks ON event_planner_bookmarks
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

CREATE TRIGGER event_planner_bookmarks_bump_version
    BEFORE UPDATE ON event_planner_bookmarks
    FOR EACH ROW EXECUTE FUNCTION bump_version();
