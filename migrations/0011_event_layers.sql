-- Phase 1 of the Porchfest Planner. Three tables that hold an
-- "event layer" (a one-off civic event like Carriage Town Porchfest),
-- the things placed on it (vendors, music porches, parking, amenities),
-- and the back-of-house task list for the people running the event.
--
-- Schema decisions:
--   * tenant_id is uuid REFERENCES tenants(id) ON DELETE CASCADE to
--     match the rest of the schema (migrations 0001..0009). The spec
--     called for TEXT, but the existing tenants table keys on uuid and
--     every other tenant-scoped table references it; staying consistent
--     prevents a foreign-key/RLS mismatch.
--   * RLS uses the existing `app.tenant_id` GUC (see
--     `crates/civic-atlas-server/src/tenant_db.rs::set_transaction_tenant`
--     and the policies on parcels/buildings in 0001). Per-call tenancy
--     is set inside a transaction by `set_config('app.tenant_id', ...)`.
--   * Geometry columns use PostGIS `geography`, per spec. For the small
--     event surface this gives WGS84-correct distance/area; the
--     existing parcels/buildings rows use `geometry` because reconstruction
--     workloads needed planar math. Both extensions are already enabled
--     by migration 0001.
--   * `category` is intentionally free-form text so the KML importer
--     can populate it without a schema lock-in. Expected values come
--     from the 2025 Porchfest map: vendor, music, parking, restroom,
--     kid_zone, food_court, rest_area, after_party, amenity. Phase 2
--     can promote these into an enum if we decide we want validation.
--   * `event_tasks.placement_id` is nullable and lands here in Phase 1
--     even though Phase 1 does not write tasks. Carrying the FK from
--     day one means Phases 2 and 3 wire the join without a migration
--     round trip.

CREATE TABLE event_layers (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    slug text NOT NULL,
    title text NOT NULL,
    starts_at timestamptz,
    ends_at timestamptz,
    bounds geography(POLYGON, 4326),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, slug)
);

CREATE TABLE event_placements (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    event_layer_id uuid NOT NULL REFERENCES event_layers(id) ON DELETE CASCADE,
    category text NOT NULL,
    sublabel text,
    label text NOT NULL,
    geometry geography(GEOMETRY, 4326) NOT NULL,
    owner_user_id uuid,
    status text NOT NULL DEFAULT 'placed',
    notes text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_event_placements_layer
    ON event_placements (tenant_id, event_layer_id);
CREATE INDEX idx_event_placements_geom
    ON event_placements USING gist (geometry);

CREATE TABLE event_tasks (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    event_layer_id uuid NOT NULL REFERENCES event_layers(id) ON DELETE CASCADE,
    title text NOT NULL,
    owner_user_id uuid,
    owner_display text,
    due_at timestamptz,
    status text NOT NULL DEFAULT 'open',
    placement_id uuid REFERENCES event_placements(id) ON DELETE SET NULL,
    notes text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_event_tasks_layer
    ON event_tasks (tenant_id, event_layer_id);
CREATE INDEX idx_event_tasks_placement
    ON event_tasks (placement_id) WHERE placement_id IS NOT NULL;
CREATE INDEX idx_event_tasks_open
    ON event_tasks (tenant_id, status) WHERE status = 'open';

ALTER TABLE event_layers ENABLE ROW LEVEL SECURITY;
ALTER TABLE event_placements ENABLE ROW LEVEL SECURITY;
ALTER TABLE event_tasks ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation_event_layers ON event_layers
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

CREATE POLICY tenant_isolation_event_placements ON event_placements
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

CREATE POLICY tenant_isolation_event_tasks ON event_tasks
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
