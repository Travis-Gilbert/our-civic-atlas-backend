CREATE TABLE IF NOT EXISTS scenarios (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  city_pack text NOT NULL CHECK (city_pack <> ''),
  scenario_id text NOT NULL CHECK (scenario_id <> ''),
  base_scenario_id text,
  name text NOT NULL CHECK (name <> ''),
  description text NOT NULL DEFAULT '',
  state text NOT NULL DEFAULT 'draft' CHECK (state IN ('draft', 'published', 'archived')),
  provenance text NOT NULL DEFAULT 'future' CHECK (
    provenance IN ('proposed', 'actual', 'historical', 'future')
  ),
  created_by text NOT NULL DEFAULT 'system' CHECK (created_by <> ''),
  published_at timestamptz,
  archived_at timestamptz,
  tags text[] NOT NULL DEFAULT '{}'::text[],
  metadata_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, city_pack, scenario_id),
  CHECK (base_scenario_id IS NULL OR base_scenario_id <> scenario_id),
  CHECK (state <> 'published' OR published_at IS NOT NULL),
  CHECK (state <> 'archived' OR archived_at IS NOT NULL)
);

INSERT INTO scenarios (
  tenant_id,
  city_pack,
  scenario_id,
  name,
  description,
  state,
  provenance,
  created_by,
  published_at
)
SELECT
  tenants.id,
  tenants.slug,
  'current',
  tenants.display_name || ' current conditions',
  'Seed scenario for present-day public atlas rows.',
  'published',
  'actual',
  'migration:0008',
  now()
FROM tenants
ON CONFLICT (tenant_id, city_pack, scenario_id) DO NOTHING;

CREATE TABLE IF NOT EXISTS scenario_zoning_overrides (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  city_pack text NOT NULL CHECK (city_pack <> ''),
  scenario_id text NOT NULL CHECK (scenario_id <> ''),
  override_id text NOT NULL CHECK (override_id <> ''),
  geom geometry(MultiPolygon, 4326) NOT NULL,
  replacement_rule_id uuid,
  rule_patch_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  note text NOT NULL DEFAULT '',
  created_by text NOT NULL DEFAULT 'system' CHECK (created_by <> ''),
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, city_pack, scenario_id, override_id),
  CHECK (
    (replacement_rule_id IS NOT NULL AND rule_patch_jsonb = '{}'::jsonb)
    OR (replacement_rule_id IS NULL AND rule_patch_jsonb <> '{}'::jsonb)
  ),
  FOREIGN KEY (tenant_id, city_pack, scenario_id)
    REFERENCES scenarios(tenant_id, city_pack, scenario_id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, replacement_rule_id)
    REFERENCES zoning_rules(tenant_id, id) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS scenario_reconstruction_overrides (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  city_pack text NOT NULL CHECK (city_pack <> ''),
  scenario_id text NOT NULL CHECK (scenario_id <> ''),
  override_id text NOT NULL CHECK (override_id <> ''),
  parcel_id uuid,
  parcel_key text NOT NULL CHECK (parcel_key <> ''),
  provenance text NOT NULL DEFAULT 'future' CHECK (
    provenance IN ('proposed', 'actual', 'historical', 'future')
  ),
  confidence double precision NOT NULL CHECK (confidence >= 0 AND confidence <= 1),
  reconstruction_spec_id text,
  reconstruction_spec_version integer CHECK (
    reconstruction_spec_version IS NULL OR reconstruction_spec_version > 0
  ),
  reconstruction_spec_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  note text NOT NULL DEFAULT '',
  created_by text NOT NULL DEFAULT 'system' CHECK (created_by <> ''),
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, city_pack, scenario_id, override_id),
  CHECK (
    (
      reconstruction_spec_id IS NOT NULL
      AND reconstruction_spec_version IS NOT NULL
      AND reconstruction_spec_jsonb = '{}'::jsonb
    )
    OR (
      reconstruction_spec_id IS NULL
      AND reconstruction_spec_version IS NULL
      AND reconstruction_spec_jsonb <> '{}'::jsonb
    )
  ),
  FOREIGN KEY (tenant_id, city_pack, scenario_id)
    REFERENCES scenarios(tenant_id, city_pack, scenario_id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, parcel_id) REFERENCES parcels(tenant_id, id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, reconstruction_spec_id, reconstruction_spec_version)
    REFERENCES reconstruction_specs(tenant_id, spec_id, version) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS scenarios_state_idx
  ON scenarios (tenant_id, city_pack, state);
CREATE INDEX IF NOT EXISTS scenarios_base_idx
  ON scenarios (tenant_id, city_pack, base_scenario_id);
CREATE INDEX IF NOT EXISTS scenarios_metadata_gin
  ON scenarios USING gin (metadata_jsonb);
CREATE INDEX IF NOT EXISTS scenarios_tags_gin
  ON scenarios USING gin (tags);

CREATE INDEX IF NOT EXISTS scenario_zoning_overrides_scenario_idx
  ON scenario_zoning_overrides (tenant_id, city_pack, scenario_id);
CREATE INDEX IF NOT EXISTS scenario_zoning_overrides_rule_idx
  ON scenario_zoning_overrides (tenant_id, replacement_rule_id);
CREATE INDEX IF NOT EXISTS scenario_zoning_overrides_geom_gix
  ON scenario_zoning_overrides USING gist (geom);
CREATE INDEX IF NOT EXISTS scenario_zoning_overrides_patch_gin
  ON scenario_zoning_overrides USING gin (rule_patch_jsonb);

CREATE INDEX IF NOT EXISTS scenario_reconstruction_overrides_scenario_idx
  ON scenario_reconstruction_overrides (tenant_id, city_pack, scenario_id);
CREATE INDEX IF NOT EXISTS scenario_reconstruction_overrides_parcel_idx
  ON scenario_reconstruction_overrides (tenant_id, parcel_key);
CREATE INDEX IF NOT EXISTS scenario_reconstruction_overrides_spec_idx
  ON scenario_reconstruction_overrides (
    tenant_id,
    reconstruction_spec_id,
    reconstruction_spec_version
  );
CREATE INDEX IF NOT EXISTS scenario_reconstruction_overrides_payload_gin
  ON scenario_reconstruction_overrides USING gin (reconstruction_spec_jsonb);

ALTER TABLE scenarios ENABLE ROW LEVEL SECURITY;
ALTER TABLE scenario_zoning_overrides ENABLE ROW LEVEL SECURITY;
ALTER TABLE scenario_reconstruction_overrides ENABLE ROW LEVEL SECURITY;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'scenarios'
      AND policyname = 'scenarios_current'
  ) THEN
    CREATE POLICY scenarios_current ON scenarios
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
      AND tablename = 'scenario_zoning_overrides'
      AND policyname = 'scenario_zoning_overrides_current'
  ) THEN
    CREATE POLICY scenario_zoning_overrides_current ON scenario_zoning_overrides
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
      AND tablename = 'scenario_reconstruction_overrides'
      AND policyname = 'scenario_reconstruction_overrides_current'
  ) THEN
    CREATE POLICY scenario_reconstruction_overrides_current ON scenario_reconstruction_overrides
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;
