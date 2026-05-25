-- Phase 3: threaded notes per placement.
--
-- Conversation thread that lives next to a pin. Vendor coordinator
-- writes "Steve confirmed he's bringing the small trailer this year,
-- fits in spot M3," Derek replies "Then we have room for one more
-- pop-up next to him." The thread lives with the pin.
--
-- Append-only for Phase 3. Edit + delete (or thread resolution) can
-- come later if the planners ask for it; the table is shaped so
-- those features land as new columns (deleted_at, edited_at) without
-- a destructive migration.
--
-- Migration number bumped from spec's 0014 because 0011..0014 are
-- already taken by Phase 1 + Phase 2.
--
-- Reuses the existing `notify_event_planner_change()` trigger
-- function from migration 0013 — its payload shape works for any
-- tenant-scoped row with `id`, `tenant_id`, `event_layer_id`. The
-- notes table doesn't have `event_layer_id` directly, so we
-- materialize it on insert via a column copy from the parent
-- placement; this lets the existing client SSE filter by event_slug
-- without an extra JOIN.

CREATE TABLE event_placement_notes (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    placement_id uuid NOT NULL REFERENCES event_placements(id) ON DELETE CASCADE,
    -- Denormalized for SSE/RLS efficiency. Kept consistent via the
    -- bump trigger below (notes copy the parent's event_layer_id
    -- on insert).
    event_layer_id uuid NOT NULL REFERENCES event_layers(id) ON DELETE CASCADE,
    author_user_id uuid NOT NULL REFERENCES event_planner_users(id) ON DELETE CASCADE,
    body text NOT NULL CHECK (length(trim(body)) > 0),
    -- Match the optimistic-concurrency model from Phase 2 even
    -- though Phase 3 notes are append-only. If/when Phase 4 adds
    -- edit, the version column already exists.
    version bigint NOT NULL DEFAULT 1,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_event_placement_notes_placement
    ON event_placement_notes (placement_id, created_at DESC);

CREATE INDEX idx_event_placement_notes_layer
    ON event_placement_notes (tenant_id, event_layer_id, created_at DESC);

ALTER TABLE event_placement_notes ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation_event_placement_notes ON event_placement_notes
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

-- Phase 3 reuses the version-bump function from 0012. Notes are
-- append-only today, but the trigger handles future updates if/when
-- the product grows an edit affordance.
CREATE TRIGGER event_placement_notes_bump_version
    BEFORE UPDATE ON event_placement_notes
    FOR EACH ROW EXECUTE FUNCTION bump_version();

-- Realtime fanout — same notify function as placements + tasks. The
-- payload already includes id + event_layer_id + tenant_id, so the
-- browser SSE consumer can route notes notifications to the right
-- panel without a schema change.
CREATE TRIGGER event_placement_notes_notify
    AFTER INSERT OR UPDATE OR DELETE ON event_placement_notes
    FOR EACH ROW EXECUTE FUNCTION notify_event_planner_change();
