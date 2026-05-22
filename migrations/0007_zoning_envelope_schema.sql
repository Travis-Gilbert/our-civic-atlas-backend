CREATE TABLE IF NOT EXISTS zoning_source_snapshots (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  city_pack text NOT NULL CHECK (city_pack <> ''),
  source_key text NOT NULL CHECK (source_key <> ''),
  source_url text NOT NULL CHECK (source_url <> ''),
  final_url text,
  source_kind text NOT NULL CHECK (
    source_kind IN ('html', 'pdf', 'arcgis-rest-metadata', 'arcgis-rest-query', 'geojson')
  ),
  retrieved_at timestamptz NOT NULL,
  content_sha256 text NOT NULL CHECK (content_sha256 ~ '^[0-9a-f]{64}$'),
  byte_count integer NOT NULL CHECK (byte_count >= 0),
  metadata_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, city_pack, source_key, content_sha256)
);

CREATE TABLE IF NOT EXISTS zoning_rules (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  city_pack text NOT NULL CHECK (city_pack <> ''),
  rule_id text NOT NULL CHECK (rule_id <> ''),
  zoning_code text NOT NULL CHECK (zoning_code <> ''),
  display_name text NOT NULL DEFAULT '',
  max_height_m double precision CHECK (max_height_m IS NULL OR max_height_m >= 0),
  max_stories double precision CHECK (max_stories IS NULL OR max_stories >= 0),
  max_far double precision CHECK (max_far IS NULL OR max_far >= 0),
  max_lot_coverage double precision CHECK (
    max_lot_coverage IS NULL OR (max_lot_coverage >= 0 AND max_lot_coverage <= 1)
  ),
  min_front_setback_m double precision CHECK (
    min_front_setback_m IS NULL OR min_front_setback_m >= 0
  ),
  min_side_setback_m double precision CHECK (
    min_side_setback_m IS NULL OR min_side_setback_m >= 0
  ),
  min_rear_setback_m double precision CHECK (
    min_rear_setback_m IS NULL OR min_rear_setback_m >= 0
  ),
  allowed_uses text[] NOT NULL DEFAULT '{}'::text[],
  conditional_uses text[] NOT NULL DEFAULT '{}'::text[],
  source_snapshot_id uuid,
  source_section text,
  valid_from date,
  valid_to date,
  confidence double precision NOT NULL DEFAULT 0.7 CHECK (confidence >= 0 AND confidence <= 1),
  rule_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, city_pack, rule_id, valid_from),
  CHECK (valid_to IS NULL OR valid_from IS NULL OR valid_to >= valid_from),
  FOREIGN KEY (tenant_id, source_snapshot_id)
    REFERENCES zoning_source_snapshots(tenant_id, id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS zoning_boundaries (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  city_pack text NOT NULL CHECK (city_pack <> ''),
  scenario_id text NOT NULL DEFAULT 'current' CHECK (scenario_id <> ''),
  parcel_id uuid,
  parcel_key text NOT NULL CHECK (parcel_key <> ''),
  pid_dash text,
  zoning_rule_id uuid NOT NULL,
  source_snapshot_id uuid,
  geom geometry(MultiPolygon, 4326) NOT NULL,
  valid_from date,
  valid_to date,
  properties_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, city_pack, scenario_id, parcel_key),
  CHECK (valid_to IS NULL OR valid_from IS NULL OR valid_to >= valid_from),
  FOREIGN KEY (tenant_id, parcel_id) REFERENCES parcels(tenant_id, id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, zoning_rule_id) REFERENCES zoning_rules(tenant_id, id) ON DELETE RESTRICT,
  FOREIGN KEY (tenant_id, source_snapshot_id)
    REFERENCES zoning_source_snapshots(tenant_id, id) ON DELETE SET NULL
);

CREATE TABLE IF NOT EXISTS buildable_envelopes (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  city_pack text NOT NULL CHECK (city_pack <> ''),
  scenario_id text NOT NULL DEFAULT 'current' CHECK (scenario_id <> ''),
  parcel_id uuid,
  parcel_key text NOT NULL CHECK (parcel_key <> ''),
  zoning_boundary_id uuid NOT NULL,
  zoning_rule_id uuid NOT NULL,
  base_geom geometry(MultiPolygon, 4326) NOT NULL,
  envelope_geom geometry(GeometryZ, 4326) NOT NULL,
  max_height_m double precision NOT NULL CHECK (max_height_m >= 0),
  max_stories double precision CHECK (max_stories IS NULL OR max_stories >= 0),
  max_far double precision,
  buildable_floor_area_m2 double precision CHECK (
    buildable_floor_area_m2 IS NULL OR buildable_floor_area_m2 >= 0
  ),
  existing_floor_area_m2 double precision CHECK (
    existing_floor_area_m2 IS NULL OR existing_floor_area_m2 >= 0
  ),
  headroom_floor_area_m2 double precision CHECK (
    headroom_floor_area_m2 IS NULL OR headroom_floor_area_m2 >= 0
  ),
  max_units_estimated integer CHECK (
    max_units_estimated IS NULL OR max_units_estimated >= 0
  ),
  binding_constraint text NOT NULL DEFAULT '',
  asset_uri text,
  content_hash text,
  warnings_jsonb jsonb NOT NULL DEFAULT '[]'::jsonb,
  metrics_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  computed_at timestamptz NOT NULL DEFAULT now(),
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, city_pack, scenario_id, parcel_key),
  FOREIGN KEY (tenant_id, parcel_id) REFERENCES parcels(tenant_id, id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, zoning_boundary_id)
    REFERENCES zoning_boundaries(tenant_id, id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, zoning_rule_id) REFERENCES zoning_rules(tenant_id, id) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS zoning_source_snapshots_city_idx
  ON zoning_source_snapshots (tenant_id, city_pack, source_key);
CREATE INDEX IF NOT EXISTS zoning_source_snapshots_metadata_gin
  ON zoning_source_snapshots USING gin (metadata_jsonb);

CREATE INDEX IF NOT EXISTS zoning_rules_code_idx
  ON zoning_rules (tenant_id, city_pack, zoning_code);
CREATE INDEX IF NOT EXISTS zoning_rules_allowed_uses_gin
  ON zoning_rules USING gin (allowed_uses);
CREATE INDEX IF NOT EXISTS zoning_rules_conditional_uses_gin
  ON zoning_rules USING gin (conditional_uses);
CREATE INDEX IF NOT EXISTS zoning_rules_rule_gin
  ON zoning_rules USING gin (rule_jsonb);

CREATE INDEX IF NOT EXISTS zoning_boundaries_rule_idx
  ON zoning_boundaries (tenant_id, zoning_rule_id);
CREATE INDEX IF NOT EXISTS zoning_boundaries_pid_dash_idx
  ON zoning_boundaries (tenant_id, pid_dash);
CREATE INDEX IF NOT EXISTS zoning_boundaries_geom_gix
  ON zoning_boundaries USING gist (geom);
CREATE INDEX IF NOT EXISTS zoning_boundaries_properties_gin
  ON zoning_boundaries USING gin (properties_jsonb);

CREATE INDEX IF NOT EXISTS buildable_envelopes_rule_idx
  ON buildable_envelopes (tenant_id, zoning_rule_id);
CREATE INDEX IF NOT EXISTS buildable_envelopes_boundary_idx
  ON buildable_envelopes (tenant_id, zoning_boundary_id);
CREATE INDEX IF NOT EXISTS buildable_envelopes_base_geom_gix
  ON buildable_envelopes USING gist (base_geom);
CREATE INDEX IF NOT EXISTS buildable_envelopes_envelope_geom_gix
  ON buildable_envelopes USING gist (envelope_geom);
CREATE INDEX IF NOT EXISTS buildable_envelopes_metrics_gin
  ON buildable_envelopes USING gin (metrics_jsonb);

ALTER TABLE zoning_source_snapshots ENABLE ROW LEVEL SECURITY;
ALTER TABLE zoning_rules ENABLE ROW LEVEL SECURITY;
ALTER TABLE zoning_boundaries ENABLE ROW LEVEL SECURITY;
ALTER TABLE buildable_envelopes ENABLE ROW LEVEL SECURITY;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'zoning_source_snapshots'
      AND policyname = 'zoning_source_snapshots_current'
  ) THEN
    CREATE POLICY zoning_source_snapshots_current ON zoning_source_snapshots
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'zoning_rules'
      AND policyname = 'zoning_rules_current'
  ) THEN
    CREATE POLICY zoning_rules_current ON zoning_rules
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'zoning_boundaries'
      AND policyname = 'zoning_boundaries_current'
  ) THEN
    CREATE POLICY zoning_boundaries_current ON zoning_boundaries
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'buildable_envelopes'
      AND policyname = 'buildable_envelopes_current'
  ) THEN
    CREATE POLICY buildable_envelopes_current ON buildable_envelopes
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;
