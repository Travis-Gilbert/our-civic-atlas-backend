-- Durable projection of run-of-show set times from the planning CRDT.
--
-- The civic planning store (Yjs civic objects) is the system of record for the
-- organizer-set set time per placement, held as free text (e.g. "14:00-14:45").
-- This table is the one-way, derived Postgres projection of the PARSED window
-- (minutes from the festival start), so the festival schedule survives as
-- queryable durable data for reporting and cross-system reads, not only as CRDT
-- free text. It is never written back to the CRDT; the projectEventSetTimes
-- mutation upserts it from the planner (CRDT -> GraphQL projection only).
--
-- Mirrors the event_email_outreach projection shape (migration 0026): tenant
-- isolation via RLS, the shared bump_version() trigger, and a per-layer unique
-- key so the planner can upsert the full schedule idempotently.

CREATE TABLE event_set_time_projections (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    event_layer_id uuid NOT NULL REFERENCES event_layers(id) ON DELETE CASCADE,
    -- Civic object provenance key (the planning store sourceId), the stable
    -- identity a placement keeps across edits. One projection per act per layer.
    source_key text NOT NULL,
    act_name text NOT NULL,
    -- Original free text the organizer entered, kept for display + audit.
    set_time_raw text,
    -- Parsed window, minutes from the festival start (the run-of-show cursor t).
    start_minute integer NOT NULL,
    end_minute integer NOT NULL,
    projected_at timestamptz NOT NULL DEFAULT now(),
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1,
    UNIQUE (tenant_id, event_layer_id, source_key),
    CHECK (end_minute >= start_minute)
);

CREATE INDEX idx_event_set_time_projections_layer
    ON event_set_time_projections (tenant_id, event_layer_id, start_minute);

CREATE TRIGGER event_set_time_projections_bump_version
    BEFORE UPDATE ON event_set_time_projections
    FOR EACH ROW EXECUTE FUNCTION bump_version();

ALTER TABLE event_set_time_projections ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation_event_set_time_projections
    ON event_set_time_projections
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
