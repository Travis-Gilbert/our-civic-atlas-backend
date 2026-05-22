CREATE OR REPLACE FUNCTION resolved_scenario_buildable_envelopes(
  requested_tenant_id uuid,
  requested_city_pack text,
  requested_scenario_id text
)
RETURNS TABLE (
  parcel_key text,
  source_scenario_id text,
  buildable_envelope_id uuid,
  zoning_rule_id uuid,
  max_height_m double precision,
  buildable_floor_area_m2 double precision,
  max_units_estimated integer,
  binding_constraint text,
  envelope_geom geometry(GeometryZ, 4326),
  metrics_jsonb jsonb
)
LANGUAGE sql
STABLE
AS $$
  WITH RECURSIVE lineage AS (
    SELECT
      scenarios.scenario_id,
      scenarios.base_scenario_id,
      0 AS depth
    FROM scenarios
    WHERE scenarios.tenant_id = requested_tenant_id
      AND scenarios.city_pack = requested_city_pack
      AND scenarios.scenario_id = requested_scenario_id

    UNION ALL

    SELECT
      parent.scenario_id,
      parent.base_scenario_id,
      lineage.depth + 1 AS depth
    FROM scenarios parent
    JOIN lineage
      ON parent.tenant_id = requested_tenant_id
     AND parent.city_pack = requested_city_pack
     AND parent.scenario_id = lineage.base_scenario_id
    WHERE lineage.base_scenario_id IS NOT NULL
  ),
  candidate_rows AS (
    SELECT
      buildable_envelopes.parcel_key,
      buildable_envelopes.scenario_id AS source_scenario_id,
      buildable_envelopes.id AS buildable_envelope_id,
      buildable_envelopes.zoning_rule_id,
      buildable_envelopes.max_height_m,
      buildable_envelopes.buildable_floor_area_m2,
      buildable_envelopes.max_units_estimated,
      buildable_envelopes.binding_constraint,
      buildable_envelopes.envelope_geom,
      buildable_envelopes.metrics_jsonb,
      lineage.depth
    FROM buildable_envelopes
    JOIN lineage
      ON buildable_envelopes.scenario_id = lineage.scenario_id
    WHERE buildable_envelopes.tenant_id = requested_tenant_id
      AND buildable_envelopes.city_pack = requested_city_pack
  )
  SELECT DISTINCT ON (candidate_rows.parcel_key)
    candidate_rows.parcel_key,
    candidate_rows.source_scenario_id,
    candidate_rows.buildable_envelope_id,
    candidate_rows.zoning_rule_id,
    candidate_rows.max_height_m,
    candidate_rows.buildable_floor_area_m2,
    candidate_rows.max_units_estimated,
    candidate_rows.binding_constraint,
    candidate_rows.envelope_geom,
    candidate_rows.metrics_jsonb
  FROM candidate_rows
  ORDER BY candidate_rows.parcel_key, candidate_rows.depth ASC;
$$;

CREATE OR REPLACE FUNCTION scenario_envelope_deltas(
  requested_tenant_id uuid,
  requested_city_pack text,
  base_scenario_id text,
  target_scenario_id text
)
RETURNS TABLE (
  parcel_key text,
  base_source_scenario_id text,
  target_source_scenario_id text,
  max_height_delta_m double precision,
  buildable_floor_area_delta_m2 double precision,
  max_units_delta integer,
  binding_constraint_changed boolean
)
LANGUAGE sql
STABLE
AS $$
  WITH base_rows AS (
    SELECT *
    FROM resolved_scenario_buildable_envelopes(
      requested_tenant_id,
      requested_city_pack,
      base_scenario_id
    )
  ),
  target_rows AS (
    SELECT *
    FROM resolved_scenario_buildable_envelopes(
      requested_tenant_id,
      requested_city_pack,
      target_scenario_id
    )
  )
  SELECT
    COALESCE(base_rows.parcel_key, target_rows.parcel_key) AS parcel_key,
    base_rows.source_scenario_id AS base_source_scenario_id,
    target_rows.source_scenario_id AS target_source_scenario_id,
    target_rows.max_height_m - base_rows.max_height_m AS max_height_delta_m,
    target_rows.buildable_floor_area_m2 - base_rows.buildable_floor_area_m2
      AS buildable_floor_area_delta_m2,
    target_rows.max_units_estimated - base_rows.max_units_estimated AS max_units_delta,
    COALESCE(base_rows.binding_constraint, '') <> COALESCE(target_rows.binding_constraint, '')
      AS binding_constraint_changed
  FROM base_rows
  FULL OUTER JOIN target_rows USING (parcel_key)
  WHERE base_rows.parcel_key IS NULL
     OR target_rows.parcel_key IS NULL
     OR target_rows.max_height_m IS DISTINCT FROM base_rows.max_height_m
     OR target_rows.buildable_floor_area_m2 IS DISTINCT FROM base_rows.buildable_floor_area_m2
     OR target_rows.max_units_estimated IS DISTINCT FROM base_rows.max_units_estimated
     OR target_rows.binding_constraint IS DISTINCT FROM base_rows.binding_constraint
  ORDER BY parcel_key;
$$;

CREATE OR REPLACE FUNCTION latest_kpi_bundle(
  requested_tenant_id uuid,
  requested_city_pack text,
  requested_scenario_id text,
  requested_scope text,
  requested_scope_id text
)
RETURNS TABLE (
  kpi_id text,
  value double precision,
  unit text,
  uncertainty_low double precision,
  uncertainty_high double precision,
  source_summary text,
  computed_at timestamptz
)
LANGUAGE sql
STABLE
AS $$
  SELECT DISTINCT ON (kpi_results.kpi_id)
    kpi_results.kpi_id,
    kpi_results.value,
    kpi_results.unit,
    kpi_results.uncertainty_low,
    kpi_results.uncertainty_high,
    kpi_results.source_summary,
    kpi_results.computed_at
  FROM kpi_results
  WHERE kpi_results.tenant_id = requested_tenant_id
    AND kpi_results.city_pack = requested_city_pack
    AND kpi_results.scenario_id = requested_scenario_id
    AND kpi_results.scope = requested_scope
    AND kpi_results.scope_id = requested_scope_id
    AND (kpi_results.expires_at IS NULL OR kpi_results.expires_at > now())
  ORDER BY kpi_results.kpi_id, kpi_results.computed_at DESC;
$$;
