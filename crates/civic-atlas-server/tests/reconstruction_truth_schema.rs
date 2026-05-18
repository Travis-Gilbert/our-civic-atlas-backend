const MIGRATION: &str = include_str!("../../../migrations/0002_reconstruction_truth_schema.sql");

const TRUTH_TABLES: [&str; 6] = [
    "building_parts",
    "artifacts",
    "artifact_anchors",
    "reconstruction_specs",
    "generated_assets",
    "corrections",
];
const OUTBOX_TABLES: [&str; 1] = ["reconstruction_projection_outbox"];

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
fn creates_required_reconstruction_truth_tables() {
    let sql = normalized_sql();

    for table in TRUTH_TABLES {
        let statement = create_table_statement(&sql, table);
        assert_contains(&statement, "tenant_id uuid not null");
    }
    for table in OUTBOX_TABLES {
        let statement = create_table_statement(&sql, table);
        assert_contains(&statement, "tenant_id uuid not null");
    }
}

#[test]
fn enables_rls_and_current_tenant_policies_for_each_truth_table() {
    let sql = normalized_sql();

    for table in TRUTH_TABLES {
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
    for table in OUTBOX_TABLES {
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
fn building_parts_keep_payload_confidence_and_source_mirrors() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "building_parts");

    assert_contains(
        &statement,
        "payload_jsonb jsonb not null default '{}'::jsonb",
    );
    assert_contains(
        &statement,
        "confidence double precision not null default 0 check (confidence >= 0 and confidence <= 1)",
    );
    assert_contains(
        &statement,
        "source_ids text[] not null default '{}'::text[]",
    );
    assert_contains(
        &sql,
        "building_parts_confidence_idx on building_parts (tenant_id, confidence)",
    );
    assert_contains(
        &sql,
        "building_parts_source_ids_gin on building_parts using gin (source_ids)",
    );
}

#[test]
fn reconstruction_specs_are_versioned_and_immutable_when_approved() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "reconstruction_specs");

    assert_contains(&statement, "version integer not null check (version > 0)");
    assert_contains(&statement, "unique (tenant_id, spec_id, version)");
    assert_contains(&sql, "old.status = 'approved'");
    assert_contains(
        &sql,
        "raise exception 'approved reconstruction_specs are immutable'",
    );
    assert_contains(
        &sql,
        "create trigger reconstruction_specs_approved_immutable_update",
    );
    assert_contains(
        &sql,
        "create trigger reconstruction_specs_approved_immutable_delete",
    );
}

#[test]
fn projection_outbox_is_idempotent_and_replayable() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "reconstruction_projection_outbox");

    assert_contains(
        &statement,
        "spec_version integer not null check (spec_version > 0)",
    );
    assert_contains(&statement, "idempotency_key text not null");
    assert_contains(&statement, "unique (tenant_id, idempotency_key)");
    assert_contains(
        &statement,
        "payload_jsonb jsonb not null default '{}'::jsonb",
    );
    assert_contains(
        &sql,
        "reconstruction_projection_outbox_status_idx on reconstruction_projection_outbox (tenant_id, status, next_attempt_at)",
    );
}

#[test]
fn migration_does_not_add_building_level_confidence_or_rustyred_truth_writes() {
    let sql = MIGRATION.to_lowercase();

    assert!(!sql.contains("building_confidence"));
    assert!(!sql.contains("rustyred"));

    for line in sql.lines() {
        let line = line.trim();
        assert!(
            !(line.contains("buildings") && line.contains("confidence")),
            "building-level confidence appears in migration line: {line}"
        );
    }
}
