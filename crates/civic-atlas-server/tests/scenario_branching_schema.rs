const MIGRATION: &str = include_str!("../../../migrations/0008_scenario_branching_schema.sql");

const SCENARIO_TABLES: [&str; 3] = [
    "scenarios",
    "scenario_zoning_overrides",
    "scenario_reconstruction_overrides",
];

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

fn create_table_statement(sql: &str, table: &str) -> String {
    let marker = format!("create table if not exists {table} (");
    let start = sql
        .find(&marker)
        .unwrap_or_else(|| panic!("missing CREATE TABLE statement for {table}"));
    let rest = &sql[start..];
    let end = rest
        .find(");")
        .unwrap_or_else(|| panic!("unterminated CREATE TABLE statement for {table}"));
    rest[..end].to_string()
}

#[test]
fn scenario_tables_are_tenant_scoped_with_rls() {
    let sql = normalized_sql();

    for table in SCENARIO_TABLES {
        let statement = create_table_statement(&sql, table);
        assert_contains(&statement, "tenant_id uuid not null");
        assert_contains(&statement, "unique (tenant_id, id)");
        assert_contains(
            &sql,
            &format!("alter table {table} enable row level security"),
        );
        assert_contains(
            &sql,
            &format!(
                "create policy {table}_current on {table} using (tenant_id::text = current_setting('app.tenant_id', true)) with check (tenant_id::text = current_setting('app.tenant_id', true))"
            ),
        );
    }
}

#[test]
fn scenarios_store_state_provenance_and_current_seed() {
    let sql = normalized_sql();
    let scenarios = create_table_statement(&sql, "scenarios");

    assert_contains(&scenarios, "scenario_id text not null");
    assert_contains(&scenarios, "base_scenario_id text");
    assert_contains(
        &scenarios,
        "state text not null default 'draft' check (state in ('draft', 'published', 'archived'))",
    );
    assert_contains(
        &scenarios,
        "provenance text not null default 'future' check ( provenance in ('proposed', 'actual', 'historical', 'future') )",
    );
    assert_contains(&scenarios, "unique (tenant_id, city_pack, scenario_id)");
    assert_contains(
        &sql,
        "insert into scenarios ( tenant_id, city_pack, scenario_id, name, description, state, provenance, created_by, published_at ) select tenants.id, tenants.slug, 'current'",
    );
}

#[test]
fn zoning_overrides_enforce_exactly_one_override_pattern() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "scenario_zoning_overrides");

    assert_contains(&statement, "geom geometry(multipolygon, 4326) not null");
    assert_contains(&statement, "replacement_rule_id uuid");
    assert_contains(
        &statement,
        "rule_patch_jsonb jsonb not null default '{}'::jsonb",
    );
    assert_contains(
        &statement,
        "(replacement_rule_id is not null and rule_patch_jsonb = '{}'::jsonb) or (replacement_rule_id is null and rule_patch_jsonb <> '{}'::jsonb)",
    );
    assert_contains(
        &statement,
        "references scenarios(tenant_id, city_pack, scenario_id) on delete cascade",
    );
    assert_contains(
        &statement,
        "references zoning_rules(tenant_id, id) on delete restrict",
    );
}

#[test]
fn reconstruction_overrides_support_reference_or_embedded_payload() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "scenario_reconstruction_overrides");

    assert_contains(&statement, "parcel_key text not null");
    assert_contains(
        &statement,
        "provenance text not null default 'future' check ( provenance in ('proposed', 'actual', 'historical', 'future') )",
    );
    assert_contains(
        &statement,
        "confidence double precision not null check (confidence >= 0 and confidence <= 1)",
    );
    assert_contains(&statement, "reconstruction_spec_id text");
    assert_contains(&statement, "reconstruction_spec_version integer");
    assert_contains(
        &statement,
        "reconstruction_spec_jsonb jsonb not null default '{}'::jsonb",
    );
    assert_contains(
        &statement,
        "references reconstruction_specs(tenant_id, spec_id, version) on delete restrict",
    );
}

#[test]
fn migration_preserves_phase_c_envelope_table_shape() {
    let sql = normalized_sql();

    assert!(!sql.contains("alter table buildable_envelopes"));
    assert!(!sql.contains("drop table buildable_envelopes"));
    assert!(!sql.contains("arcgis urban"));
    assert!(!sql.contains("cityengine"));
    assert!(!sql.contains("arcgis runtime"));
}
