const MIGRATION: &str = include_str!("../../../migrations/0009_kpi_service_schema.sql");

const KPI_TABLES: [&str; 4] = [
    "multipliers",
    "kpi_definitions",
    "demographics_baselines",
    "kpi_results",
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
fn kpi_tables_are_tenant_scoped_with_rls() {
    let sql = normalized_sql();

    for table in KPI_TABLES {
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
fn multipliers_keep_cited_source_context_and_uncertainty() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "multipliers");

    assert_contains(&statement, "multiplier_id text not null");
    assert_contains(&statement, "value double precision not null");
    assert_contains(&statement, "unit text not null");
    assert_contains(
        &statement,
        "applies_to text[] not null default '{}'::text[]",
    );
    assert_contains(&statement, "uncertainty_low double precision");
    assert_contains(&statement, "uncertainty_high double precision");
    assert_contains(&statement, "source_name text not null");
    assert_contains(&statement, "source_url text not null");
    assert_contains(&statement, "source_vintage text not null");
    assert_contains(
        &statement,
        "citation_jsonb jsonb not null default '{}'::jsonb",
    );
    assert_contains(
        &statement,
        "unique (tenant_id, city_pack, multiplier_id, source_vintage)",
    );
}

#[test]
fn kpi_definitions_store_formula_as_data() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "kpi_definitions");

    assert_contains(&statement, "kpi_id text not null");
    assert_contains(
        &statement,
        "scope text not null check (scope in ('parcel', 'block', 'ward', 'city'))",
    );
    assert_contains(&statement, "formula text not null check (formula <> '')");
    assert_contains(
        &statement,
        "required_multipliers text[] not null default '{}'::text[]",
    );
    assert_contains(&statement, "source_note text not null");
    assert_contains(&statement, "unique (tenant_id, city_pack, kpi_id, scope)");
}

#[test]
fn baselines_and_results_are_scenario_and_scope_aware() {
    let sql = normalized_sql();
    let baselines = create_table_statement(&sql, "demographics_baselines");
    let results = create_table_statement(&sql, "kpi_results");

    for statement in [&baselines, &results] {
        assert_contains(statement, "scope text not null");
        assert_contains(statement, "scope_id text not null");
        assert_contains(statement, "value double precision not null");
        assert_contains(statement, "unit text not null");
        assert_contains(statement, "uncertainty_low double precision");
        assert_contains(statement, "uncertainty_high double precision");
    }

    assert_contains(&baselines, "metric_id text not null");
    assert_contains(&baselines, "observed_at date not null");
    assert_contains(
        &results,
        "scenario_id text not null check (scenario_id <> '')",
    );
    assert_contains(&results, "kpi_id text not null");
    assert_contains(
        &results,
        "inputs_hash text not null check (inputs_hash ~ '^[0-9a-f]{64}$')",
    );
    assert_contains(&results, "source_summary text not null");
    assert_contains(
        &results,
        "references scenarios(tenant_id, city_pack, scenario_id) on delete cascade",
    );
    assert_contains(
        &results,
        "references kpi_definitions(tenant_id, city_pack, kpi_id, scope) on delete restrict",
    );
}

#[test]
fn migration_keeps_kpi_work_behind_backend_boundary() {
    let sql = normalized_sql();

    assert!(!sql.contains("openai"));
    assert!(!sql.contains("firecrawl"));
    assert!(!sql.contains("next.js route handler"));
    assert!(!sql.contains("arcgis urban"));
}
