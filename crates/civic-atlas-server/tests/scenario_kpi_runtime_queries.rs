const MIGRATION: &str = include_str!("../../../migrations/0010_scenario_kpi_runtime_queries.sql");

fn normalized_sql() -> String {
    MIGRATION
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn assert_contains(sql: &str, expected: &str) {
    assert!(
        sql.contains(expected),
        "migration is missing expected SQL fragment: {expected}"
    );
}

#[test]
fn resolved_envelopes_follow_scenario_lineage() {
    let sql = normalized_sql();

    assert_contains(
        &sql,
        "create or replace function resolved_scenario_buildable_envelopes",
    );
    assert_contains(&sql, "with recursive lineage as");
    assert_contains(&sql, "select distinct on (candidate_rows.parcel_key)");
    assert_contains(
        &sql,
        "order by candidate_rows.parcel_key, candidate_rows.depth asc",
    );
    assert_contains(&sql, "from buildable_envelopes");
}

#[test]
fn scenario_delta_query_compares_resolved_envelopes() {
    let sql = normalized_sql();

    assert_contains(&sql, "create or replace function scenario_envelope_deltas");
    assert_contains(
        &sql,
        "from resolved_scenario_buildable_envelopes( requested_tenant_id, requested_city_pack, base_scenario_id )",
    );
    assert_contains(
        &sql,
        "from resolved_scenario_buildable_envelopes( requested_tenant_id, requested_city_pack, target_scenario_id )",
    );
    assert_contains(&sql, "full outer join target_rows using (parcel_key)");
    assert_contains(&sql, "binding_constraint_changed");
}

#[test]
fn latest_kpi_bundle_returns_freshest_unexpired_metric_rows() {
    let sql = normalized_sql();

    assert_contains(&sql, "create or replace function latest_kpi_bundle");
    assert_contains(&sql, "select distinct on (kpi_results.kpi_id)");
    assert_contains(
        &sql,
        "and (kpi_results.expires_at is null or kpi_results.expires_at > now())",
    );
    assert_contains(
        &sql,
        "order by kpi_results.kpi_id, kpi_results.computed_at desc",
    );
}
