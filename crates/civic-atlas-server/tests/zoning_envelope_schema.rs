const MIGRATION: &str = include_str!("../../../migrations/0007_zoning_envelope_schema.sql");

const ZONING_TABLES: [&str; 4] = [
    "zoning_source_snapshots",
    "zoning_rules",
    "zoning_boundaries",
    "buildable_envelopes",
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
fn zoning_tables_are_tenant_scoped() {
    let sql = normalized_sql();

    for table in ZONING_TABLES {
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
fn source_snapshots_record_public_source_hashes() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "zoning_source_snapshots");

    assert_contains(&statement, "city_pack text not null");
    assert_contains(&statement, "source_key text not null");
    assert_contains(&statement, "source_url text not null");
    assert_contains(&statement, "retrieved_at timestamptz not null");
    assert_contains(
        &statement,
        "content_sha256 text not null check (content_sha256 ~ '^[0-9a-f]{64}$')",
    );
    assert_contains(
        &statement,
        "source_kind in ('html', 'pdf', 'arcgis-rest-metadata', 'arcgis-rest-query', 'geojson')",
    );
    assert_contains(
        &statement,
        "unique (tenant_id, city_pack, source_key, content_sha256)",
    );
}

#[test]
fn zoning_rules_capture_massing_uses_and_validity() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "zoning_rules");

    assert_contains(&statement, "rule_id text not null");
    assert_contains(&statement, "zoning_code text not null");
    assert_contains(&statement, "max_height_m double precision");
    assert_contains(&statement, "max_stories double precision");
    assert_contains(&statement, "max_far double precision");
    assert_contains(
        &statement,
        "allowed_uses text[] not null default '{}'::text[]",
    );
    assert_contains(
        &statement,
        "conditional_uses text[] not null default '{}'::text[]",
    );
    assert_contains(&statement, "source_snapshot_id uuid");
    assert_contains(&statement, "valid_from date");
    assert_contains(&statement, "valid_to date");
    assert_contains(
        &statement,
        "confidence double precision not null default 0.7 check (confidence >= 0 and confidence <= 1)",
    );
    assert_contains(
        &statement,
        "unique (tenant_id, city_pack, rule_id, valid_from)",
    );
}

#[test]
fn boundaries_and_envelopes_are_scenario_ready() {
    let sql = normalized_sql();
    let boundaries = create_table_statement(&sql, "zoning_boundaries");
    let envelopes = create_table_statement(&sql, "buildable_envelopes");

    for statement in [&boundaries, &envelopes] {
        assert_contains(statement, "city_pack text not null");
        assert_contains(
            statement,
            "scenario_id text not null default 'current' check (scenario_id <> '')",
        );
        assert_contains(
            statement,
            "parcel_key text not null check (parcel_key <> '')",
        );
        assert_contains(statement, "zoning_rule_id uuid not null");
        assert_contains(
            statement,
            "unique (tenant_id, city_pack, scenario_id, parcel_key)",
        );
    }

    assert_contains(&boundaries, "geom geometry(multipolygon, 4326) not null");
    assert_contains(
        &envelopes,
        "base_geom geometry(multipolygon, 4326) not null",
    );
    assert_contains(
        &envelopes,
        "envelope_geom geometry(geometryz, 4326) not null",
    );
    assert_contains(&envelopes, "max_height_m double precision not null");
    assert_contains(&envelopes, "max_stories double precision");
    assert_contains(&envelopes, "headroom_floor_area_m2 double precision");
    assert_contains(&envelopes, "max_units_estimated integer");
    assert_contains(&envelopes, "binding_constraint text not null default ''");
    assert_contains(&envelopes, "asset_uri text");
    assert_contains(
        &envelopes,
        "metrics_jsonb jsonb not null default '{}'::jsonb",
    );
}

#[test]
fn migration_does_not_depend_on_esri_products() {
    let sql = MIGRATION.to_lowercase();

    assert!(!sql.contains("arcgis urban"));
    assert!(!sql.contains("cityengine"));
    assert!(!sql.contains("arcgis runtime"));
    assert!(!sql.contains("esri sdk"));
}
