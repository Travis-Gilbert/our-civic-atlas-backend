-- Backfill temporal fields on the Carriage Town seed so that
-- /open-flint-atlas/atelier/<parcelId>/<year> routes resolve against
-- real time anchors instead of NULL columns and empty payload_jsonb.
--
-- Touches three layers (NOT reconstruction_specs: approved specs are
-- immutable by trigger, and time data belongs on buildings anyway):
--   1. buildings.t_start_ms / t_end_ms per real lifespan (the 0004 seed
--      hardcoded 1885 for every building, which is only correct for
--      Whaley House).
--   2. artifacts.payload_jsonb.year and captured_at_ms — the year the
--      artifact's claim represents (1899 for the Sanborn sheet, 1908
--      for the Whaley photo, 1925 for the storefront photo).
--   3. artifact_anchors.t_start_ms / t_end_ms — derived from the
--      artifact year so the engine's recency-weighted merge has
--      temporal grounding.
--
-- Time range on reconstruction_specs is intentionally NOT touched
-- here. Specs carry material/form/use; lifespan is a building-level
-- fact and any UI that needs to render a lifespan badge for a spec
-- joins through buildings.t_start_ms / t_end_ms.
--
-- All UPDATEs are idempotent: filtered by tenant slug and stable
-- artifact_key / civic_object_id values from 0004.
--
-- This migration was triggered by the 2026-05-26 live PostGIS audit
-- (see docs/plans/evidence-corpus-inventory-2026-05.md "Verified
-- against live PostGIS"). The audit found every payload_jsonb empty
-- {} and every t_start_ms / t_end_ms NULL on seeded anchors.

DO $$
DECLARE
  flint_tenant_id uuid;
BEGIN
  SELECT id INTO flint_tenant_id FROM tenants WHERE slug = 'flint';
  IF flint_tenant_id IS NULL THEN
    RAISE NOTICE 'flint tenant missing; skipping temporal backfill';
    RETURN;
  END IF;

  PERFORM set_config('app.tenant_id', flint_tenant_id::text, true);

  -- 1. Per-building lifespans (carriage-town:1 through :5).
  --
  -- Lifespans match the public app fixture at
  -- public/atlas/historical/carriage-town.json and the inventory doc.
  -- 1: Whaley House          1885 to present
  -- 2: 628 E Kearsley Frame  1892 to present
  -- 3: Carriage Town Store   1905 to 1968 (demolished)
  -- 4: Worker's Cottage      1898 to 1962 (demolished)
  -- 5: Stockton House        1872 to 1955 (demolished)
  UPDATE buildings SET
    t_start_ms = EXTRACT(EPOCH FROM TIMESTAMP '1885-01-01 00:00:00Z')::bigint * 1000,
    t_end_ms = NULL
  WHERE tenant_id = flint_tenant_id AND civic_object_id = 'building:carriage-town:1';

  UPDATE buildings SET
    t_start_ms = EXTRACT(EPOCH FROM TIMESTAMP '1892-01-01 00:00:00Z')::bigint * 1000,
    t_end_ms = NULL
  WHERE tenant_id = flint_tenant_id AND civic_object_id = 'building:carriage-town:2';

  UPDATE buildings SET
    t_start_ms = EXTRACT(EPOCH FROM TIMESTAMP '1905-01-01 00:00:00Z')::bigint * 1000,
    t_end_ms = EXTRACT(EPOCH FROM TIMESTAMP '1968-12-31 23:59:59Z')::bigint * 1000
  WHERE tenant_id = flint_tenant_id AND civic_object_id = 'building:carriage-town:3';

  UPDATE buildings SET
    t_start_ms = EXTRACT(EPOCH FROM TIMESTAMP '1898-01-01 00:00:00Z')::bigint * 1000,
    t_end_ms = EXTRACT(EPOCH FROM TIMESTAMP '1962-12-31 23:59:59Z')::bigint * 1000
  WHERE tenant_id = flint_tenant_id AND civic_object_id = 'building:carriage-town:4';

  UPDATE buildings SET
    t_start_ms = EXTRACT(EPOCH FROM TIMESTAMP '1872-01-01 00:00:00Z')::bigint * 1000,
    t_end_ms = EXTRACT(EPOCH FROM TIMESTAMP '1955-12-31 23:59:59Z')::bigint * 1000
  WHERE tenant_id = flint_tenant_id AND civic_object_id = 'building:carriage-town:5';

  -- 2. Artifact payload year. A photograph or map sheet is a snapshot;
  -- the year is the moment its claim is true.
  UPDATE artifacts SET
    payload_jsonb = payload_jsonb || jsonb_build_object('year', 1908, 'captured_kind', 'photograph'),
    captured_at_ms = EXTRACT(EPOCH FROM TIMESTAMP '1908-01-01 00:00:00Z')::bigint * 1000
  WHERE tenant_id = flint_tenant_id AND artifact_key = 'artifact:whaley-photo-1908';

  UPDATE artifacts SET
    payload_jsonb = payload_jsonb || jsonb_build_object('year', 1899, 'captured_kind', 'sanborn_sheet'),
    captured_at_ms = EXTRACT(EPOCH FROM TIMESTAMP '1899-01-01 00:00:00Z')::bigint * 1000
  WHERE tenant_id = flint_tenant_id AND artifact_key = 'artifact:carriage-sanborn-1899';

  UPDATE artifacts SET
    payload_jsonb = payload_jsonb || jsonb_build_object('year', 1925, 'captured_kind', 'photograph'),
    captured_at_ms = EXTRACT(EPOCH FROM TIMESTAMP '1925-01-01 00:00:00Z')::bigint * 1000
  WHERE tenant_id = flint_tenant_id AND artifact_key = 'artifact:storefront-photo-1925';

  -- 3. Anchor temporal bounds derived from artifact year.
  -- An instant artifact (photo, map snapshot) brackets its claim to
  -- the calendar year it was made; the engine's recency-weighted
  -- merge can then locate the anchor in time. t_start_ms = January 1
  -- of the year, t_end_ms = December 31 of the year.
  UPDATE artifact_anchors aa SET
    t_start_ms = EXTRACT(EPOCH FROM TIMESTAMP '1908-01-01 00:00:00Z')::bigint * 1000,
    t_end_ms = EXTRACT(EPOCH FROM TIMESTAMP '1908-12-31 23:59:59Z')::bigint * 1000
  FROM artifacts a
  WHERE aa.tenant_id = flint_tenant_id
    AND a.id = aa.artifact_id
    AND a.tenant_id = aa.tenant_id
    AND a.artifact_key = 'artifact:whaley-photo-1908';

  UPDATE artifact_anchors aa SET
    t_start_ms = EXTRACT(EPOCH FROM TIMESTAMP '1899-01-01 00:00:00Z')::bigint * 1000,
    t_end_ms = EXTRACT(EPOCH FROM TIMESTAMP '1899-12-31 23:59:59Z')::bigint * 1000
  FROM artifacts a
  WHERE aa.tenant_id = flint_tenant_id
    AND a.id = aa.artifact_id
    AND a.tenant_id = aa.tenant_id
    AND a.artifact_key = 'artifact:carriage-sanborn-1899';

  UPDATE artifact_anchors aa SET
    t_start_ms = EXTRACT(EPOCH FROM TIMESTAMP '1925-01-01 00:00:00Z')::bigint * 1000,
    t_end_ms = EXTRACT(EPOCH FROM TIMESTAMP '1925-12-31 23:59:59Z')::bigint * 1000
  FROM artifacts a
  WHERE aa.tenant_id = flint_tenant_id
    AND a.id = aa.artifact_id
    AND a.tenant_id = aa.tenant_id
    AND a.artifact_key = 'artifact:storefront-photo-1925';

END
$$;
