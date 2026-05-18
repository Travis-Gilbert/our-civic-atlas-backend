-- Geographic claim triangulation: tenant bbox + civic_districts +
-- provenance disputes ledger.
--
-- Rationale: a name/coord mismatch (the Hubbard Drug Carriage Town
-- bug — fixture coord at lat 43.0185 in Civic Park, description
-- claiming Carriage Town historic district at lat ~43.013) is the
-- failure mode of trusting one claim type. This migration adds the
-- substrate for cheap cross-validation:
--
--   1. `tenants.bbox`         — every place's coord must fall inside.
--                                Cross-tenant errors caught O(1).
--   2. `civic_districts`      — named district polygons. A claim
--                                that references "Carriage Town"
--                                gets point-in-polygon checked
--                                against the district's bounds.
--   3. `place_provenance_disputes` — append-only ledger of every
--                                automatic disagreement the system
--                                detected. Moderators see them;
--                                ACC/ACT will train on them.
--
-- Validators in `crates/civic-atlas-server/src/validation.rs` read
-- these tables at write-time (approve_spec, correction approval,
-- corpus ingest) and emit dispute rows when claims disagree.

-- 1. tenants.bbox -----------------------------------------------------

ALTER TABLE tenants
  ADD COLUMN IF NOT EXISTS bbox geometry(Polygon, 4326);

-- Seed Flint's bounding region. City limits roughly:
--   W = -83.795, E = -83.595, S = 42.965, N = 43.085
-- These match the bbox the OSM fetcher script used to pull the
-- 21,182-building dataset (scripts/fetch-osm-buildings.mjs in the
-- frontend). The two should stay in sync; this is the
-- authoritative source.
UPDATE tenants
   SET bbox = ST_MakeEnvelope(-83.795, 42.965, -83.595, 43.085, 4326)
 WHERE slug = 'flint'
   AND bbox IS NULL;

-- 2. civic_districts -------------------------------------------------

CREATE TABLE IF NOT EXISTS civic_districts (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  slug text NOT NULL CHECK (slug ~ '^[a-z0-9-]+$'),
  display_name text NOT NULL,
  polygon geometry(MultiPolygon, 4326) NOT NULL,
  description text NOT NULL DEFAULT '',
  -- Source of the polygon: 'osm', 'municipal_gis', 'historic_register',
  -- 'hand_authored'. Lets the validator weight disagreement severity.
  source_kind text NOT NULL DEFAULT 'hand_authored'
    CHECK (source_kind <> ''),
  source_citation text NOT NULL DEFAULT '',
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, slug)
);

CREATE INDEX IF NOT EXISTS civic_districts_polygon_gix
  ON civic_districts USING gist (polygon);

CREATE INDEX IF NOT EXISTS civic_districts_tenant_slug_idx
  ON civic_districts (tenant_id, slug);

ALTER TABLE civic_districts ENABLE ROW LEVEL SECURITY;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'civic_districts'
      AND policyname = 'civic_districts_current'
  ) THEN
    CREATE POLICY civic_districts_current ON civic_districts
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

-- Seed Carriage Town historic district. Polygon approximates the
-- registered boundaries: bounded by Grand Traverse (E), Beach (W),
-- Flint River (N), I-475 (S). The actual district is a rectangle
-- with the river bend cutting the NE corner; for MVP we use the
-- envelope rectangle. Replace with the authoritative OSM polygon
-- when civic-atlas-ingest pulls administrative boundaries.
INSERT INTO civic_districts (
  tenant_id, slug, display_name, polygon, description, source_kind, source_citation
)
SELECT
  t.id,
  'carriage-town',
  'Carriage Town Historic District',
  ST_Multi(ST_GeomFromText(
    'POLYGON((-83.711 43.005, -83.706 43.005, -83.706 43.014, -83.711 43.014, -83.711 43.005))',
    4326
  )),
  'Registered historic district bounded by Grand Traverse Street (east), Beach Street (west), the Flint River (north), and I-475 (south).',
  'hand_authored',
  'Flint Carriage Town Historic District boundary description; replace with OSM polygon when ingestion fetches admin_level=10.'
FROM tenants t
WHERE t.slug = 'flint'
ON CONFLICT (tenant_id, slug) DO NOTHING;

-- 3. place_provenance_disputes ---------------------------------------

CREATE TABLE IF NOT EXISTS place_provenance_disputes (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  -- The entity the dispute is about. Polymorphic; matches the same
  -- shape as the `corrections` table for symmetry.
  target_type text NOT NULL CHECK (
    target_type IN (
      'building',
      'parcel',
      'building_part',
      'reconstruction_spec'
    )
  ),
  target_id uuid NOT NULL,
  -- Which validator caught it.
  dispute_kind text NOT NULL CHECK (dispute_kind <> ''),
  severity text NOT NULL DEFAULT 'flag' CHECK (
    severity IN ('flag', 'warn', 'block')
  ),
  -- Human-readable explanation of what disagreed with what. Should
  -- name BOTH claims so a moderator can read it without code.
  evidence_text text NOT NULL CHECK (evidence_text <> ''),
  -- Optional JSONB for structured evidence (the two coords, the
  -- district name, the address, etc.). Let validators be additive
  -- in what they record without schema changes.
  evidence_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  detected_at timestamptz NOT NULL DEFAULT now(),
  -- Resolution audit trail. Empty until a moderator acts.
  resolved_at timestamptz,
  resolved_by text NOT NULL DEFAULT '',
  resolution_kind text NOT NULL DEFAULT '' CHECK (
    resolution_kind IN ('', 'confirmed', 'overridden', 'corrected', 'duplicate')
  ),
  resolution_note text NOT NULL DEFAULT ''
);

CREATE INDEX IF NOT EXISTS provenance_disputes_target_idx
  ON place_provenance_disputes (tenant_id, target_type, target_id);

CREATE INDEX IF NOT EXISTS provenance_disputes_open_idx
  ON place_provenance_disputes (tenant_id, dispute_kind, detected_at)
  WHERE resolved_at IS NULL;

CREATE INDEX IF NOT EXISTS provenance_disputes_evidence_gin
  ON place_provenance_disputes USING gin (evidence_jsonb);

ALTER TABLE place_provenance_disputes ENABLE ROW LEVEL SECURITY;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'place_provenance_disputes'
      AND policyname = 'place_provenance_disputes_current'
  ) THEN
    CREATE POLICY place_provenance_disputes_current ON place_provenance_disputes
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

-- Immutability once resolved. Re-opening a dispute should create a
-- new row, not mutate the audit trail.
CREATE OR REPLACE FUNCTION prevent_resolved_dispute_mutation()
RETURNS trigger AS $$
BEGIN
  IF OLD.resolved_at IS NOT NULL THEN
    IF NEW.resolved_at IS NULL
       OR NEW.resolution_kind <> OLD.resolution_kind
       OR NEW.resolved_by <> OLD.resolved_by THEN
      RAISE EXCEPTION 'resolved provenance disputes are immutable; create a new row for re-opens';
    END IF;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS provenance_disputes_resolved_immutable_update
  ON place_provenance_disputes;
CREATE TRIGGER provenance_disputes_resolved_immutable_update
  BEFORE UPDATE ON place_provenance_disputes
  FOR EACH ROW
  EXECUTE FUNCTION prevent_resolved_dispute_mutation();
