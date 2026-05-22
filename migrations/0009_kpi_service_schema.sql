CREATE TABLE IF NOT EXISTS multipliers (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  city_pack text NOT NULL CHECK (city_pack <> ''),
  multiplier_id text NOT NULL CHECK (multiplier_id <> ''),
  display_name text NOT NULL DEFAULT '',
  value double precision NOT NULL CHECK (value = value),
  unit text NOT NULL CHECK (unit <> ''),
  applies_to text[] NOT NULL DEFAULT '{}'::text[],
  uncertainty_low double precision CHECK (uncertainty_low IS NULL OR uncertainty_low = uncertainty_low),
  uncertainty_high double precision CHECK (
    uncertainty_high IS NULL OR uncertainty_high = uncertainty_high
  ),
  source_name text NOT NULL CHECK (source_name <> ''),
  source_url text NOT NULL CHECK (source_url <> ''),
  source_vintage text NOT NULL CHECK (source_vintage <> ''),
  valid_from date,
  valid_to date,
  citation_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, city_pack, multiplier_id, source_vintage),
  CHECK (
    uncertainty_low IS NULL
    OR uncertainty_high IS NULL
    OR uncertainty_high >= uncertainty_low
  ),
  CHECK (valid_to IS NULL OR valid_from IS NULL OR valid_to >= valid_from)
);

CREATE TABLE IF NOT EXISTS kpi_definitions (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  city_pack text NOT NULL CHECK (city_pack <> ''),
  kpi_id text NOT NULL CHECK (kpi_id <> ''),
  scope text NOT NULL CHECK (scope IN ('parcel', 'block', 'ward', 'city')),
  display_name text NOT NULL CHECK (display_name <> ''),
  description text NOT NULL DEFAULT '',
  unit text NOT NULL CHECK (unit <> ''),
  formula text NOT NULL CHECK (formula <> ''),
  required_multipliers text[] NOT NULL DEFAULT '{}'::text[],
  source_note text NOT NULL CHECK (source_note <> ''),
  precision integer NOT NULL DEFAULT 2 CHECK (precision >= 0 AND precision <= 8),
  active boolean NOT NULL DEFAULT true,
  definition_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, city_pack, kpi_id, scope)
);

CREATE TABLE IF NOT EXISTS demographics_baselines (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  city_pack text NOT NULL CHECK (city_pack <> ''),
  scope text NOT NULL CHECK (scope IN ('parcel', 'block', 'ward', 'city')),
  scope_id text NOT NULL CHECK (scope_id <> ''),
  metric_id text NOT NULL CHECK (metric_id <> ''),
  value double precision NOT NULL CHECK (value = value),
  unit text NOT NULL CHECK (unit <> ''),
  uncertainty_low double precision CHECK (uncertainty_low IS NULL OR uncertainty_low = uncertainty_low),
  uncertainty_high double precision CHECK (
    uncertainty_high IS NULL OR uncertainty_high = uncertainty_high
  ),
  source_name text NOT NULL CHECK (source_name <> ''),
  source_url text NOT NULL CHECK (source_url <> ''),
  source_vintage text NOT NULL CHECK (source_vintage <> ''),
  observed_at date NOT NULL,
  baseline_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, city_pack, scope, scope_id, metric_id, source_vintage),
  CHECK (
    uncertainty_low IS NULL
    OR uncertainty_high IS NULL
    OR uncertainty_high >= uncertainty_low
  )
);

CREATE TABLE IF NOT EXISTS kpi_results (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  city_pack text NOT NULL CHECK (city_pack <> ''),
  scenario_id text NOT NULL CHECK (scenario_id <> ''),
  scope text NOT NULL CHECK (scope IN ('parcel', 'block', 'ward', 'city')),
  scope_id text NOT NULL CHECK (scope_id <> ''),
  kpi_id text NOT NULL CHECK (kpi_id <> ''),
  value double precision NOT NULL CHECK (value = value),
  unit text NOT NULL CHECK (unit <> ''),
  uncertainty_low double precision CHECK (uncertainty_low IS NULL OR uncertainty_low = uncertainty_low),
  uncertainty_high double precision CHECK (
    uncertainty_high IS NULL OR uncertainty_high = uncertainty_high
  ),
  inputs_hash text NOT NULL CHECK (inputs_hash ~ '^[0-9a-f]{64}$'),
  source_summary text NOT NULL CHECK (source_summary <> ''),
  result_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  computed_at timestamptz NOT NULL DEFAULT now(),
  expires_at timestamptz,
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, city_pack, scenario_id, scope, scope_id, kpi_id, inputs_hash),
  CHECK (
    uncertainty_low IS NULL
    OR uncertainty_high IS NULL
    OR uncertainty_high >= uncertainty_low
  ),
  CHECK (uncertainty_low IS NULL OR value >= uncertainty_low),
  CHECK (uncertainty_high IS NULL OR value <= uncertainty_high),
  FOREIGN KEY (tenant_id, city_pack, scenario_id)
    REFERENCES scenarios(tenant_id, city_pack, scenario_id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, city_pack, kpi_id, scope)
    REFERENCES kpi_definitions(tenant_id, city_pack, kpi_id, scope) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS multipliers_city_idx
  ON multipliers (tenant_id, city_pack, multiplier_id);
CREATE INDEX IF NOT EXISTS multipliers_applies_to_gin
  ON multipliers USING gin (applies_to);
CREATE INDEX IF NOT EXISTS multipliers_citation_gin
  ON multipliers USING gin (citation_jsonb);

CREATE INDEX IF NOT EXISTS kpi_definitions_scope_idx
  ON kpi_definitions (tenant_id, city_pack, scope, active);
CREATE INDEX IF NOT EXISTS kpi_definitions_required_multipliers_gin
  ON kpi_definitions USING gin (required_multipliers);
CREATE INDEX IF NOT EXISTS kpi_definitions_definition_gin
  ON kpi_definitions USING gin (definition_jsonb);

CREATE INDEX IF NOT EXISTS demographics_baselines_scope_idx
  ON demographics_baselines (tenant_id, city_pack, scope, scope_id);
CREATE INDEX IF NOT EXISTS demographics_baselines_metric_idx
  ON demographics_baselines (tenant_id, city_pack, metric_id);
CREATE INDEX IF NOT EXISTS demographics_baselines_payload_gin
  ON demographics_baselines USING gin (baseline_jsonb);

CREATE INDEX IF NOT EXISTS kpi_results_scenario_scope_idx
  ON kpi_results (tenant_id, city_pack, scenario_id, scope, scope_id);
CREATE INDEX IF NOT EXISTS kpi_results_kpi_idx
  ON kpi_results (tenant_id, city_pack, kpi_id);
CREATE INDEX IF NOT EXISTS kpi_results_computed_idx
  ON kpi_results (tenant_id, computed_at);
CREATE INDEX IF NOT EXISTS kpi_results_payload_gin
  ON kpi_results USING gin (result_jsonb);

ALTER TABLE multipliers ENABLE ROW LEVEL SECURITY;
ALTER TABLE kpi_definitions ENABLE ROW LEVEL SECURITY;
ALTER TABLE demographics_baselines ENABLE ROW LEVEL SECURITY;
ALTER TABLE kpi_results ENABLE ROW LEVEL SECURITY;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'multipliers'
      AND policyname = 'multipliers_current'
  ) THEN
    CREATE POLICY multipliers_current ON multipliers
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
      AND tablename = 'kpi_definitions'
      AND policyname = 'kpi_definitions_current'
  ) THEN
    CREATE POLICY kpi_definitions_current ON kpi_definitions
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
      AND tablename = 'demographics_baselines'
      AND policyname = 'demographics_baselines_current'
  ) THEN
    CREATE POLICY demographics_baselines_current ON demographics_baselines
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
      AND tablename = 'kpi_results'
      AND policyname = 'kpi_results_current'
  ) THEN
    CREATE POLICY kpi_results_current ON kpi_results
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;
