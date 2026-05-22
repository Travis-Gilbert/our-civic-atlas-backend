-- Phase 2 realtime fanout: row-level pg_notify on every change to
-- event_placements or event_tasks. The Node GraphQL sidecar opens
-- one long-lived `pg.Client` (separate from the connection pool),
-- runs `LISTEN event_planner_<tenant_slug>`, and pushes each
-- notification out to connected SSE clients.
--
-- Why tenant-keyed channels: a single notify trigger that fanned out
-- to one channel would force every tenant's sidecar to filter on the
-- payload. Per-tenant channels mean the LISTEN itself filters, and
-- payload bytes stay small (8 KB ceiling per notification).
--
-- The channel uses the tenant's *slug*, not its uuid id, because the
-- sidecar already knows the slug it's serving ("flint") and the
-- channel needs to be a stable identifier across redeploys. We
-- resolve uuid -> slug inside the trigger via the tenants table.
--
-- Payload only carries IDs + op + table. Clients refetch the affected
-- row by id when they care about the new contents. This keeps the
-- payload comfortably under the 8 KB ceiling and means the SSE
-- connection isn't a separate trust surface.
--
-- Migration number bumped from spec's 0012 (versions took that slot).

CREATE OR REPLACE FUNCTION notify_event_planner_change() RETURNS trigger AS $$
DECLARE
    payload jsonb;
    channel text;
    tenant_slug text;
    target_tenant_id uuid;
BEGIN
    target_tenant_id := COALESCE(NEW.tenant_id, OLD.tenant_id);
    SELECT slug INTO tenant_slug
    FROM tenants
    WHERE id = target_tenant_id;

    -- Defensive: if the tenant row is gone (shouldn't happen because
    -- the FK cascades, but the trigger runs even for cascade deletes),
    -- skip the notify rather than fail the transaction.
    IF tenant_slug IS NULL THEN
        RETURN COALESCE(NEW, OLD);
    END IF;

    channel := 'event_planner_' || tenant_slug;
    payload := jsonb_build_object(
        'op', TG_OP,
        'table', TG_TABLE_NAME,
        'id', COALESCE(NEW.id, OLD.id),
        'event_layer_id', COALESCE(NEW.event_layer_id, OLD.event_layer_id),
        'tenant_id', target_tenant_id,
        'version', COALESCE(NEW.version, OLD.version)
    );
    PERFORM pg_notify(channel, payload::text);
    RETURN COALESCE(NEW, OLD);
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER event_placements_notify
    AFTER INSERT OR UPDATE OR DELETE ON event_placements
    FOR EACH ROW EXECUTE FUNCTION notify_event_planner_change();

CREATE TRIGGER event_tasks_notify
    AFTER INSERT OR UPDATE OR DELETE ON event_tasks
    FOR EACH ROW EXECUTE FUNCTION notify_event_planner_change();
