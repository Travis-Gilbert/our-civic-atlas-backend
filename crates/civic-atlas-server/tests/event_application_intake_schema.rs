const MIGRATION: &str = include_str!("../../../migrations/0022_event_application_intake.sql");

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
    let marker = format!("create table {table} (");
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
fn application_intake_is_tenant_scoped_and_idempotent() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "event_applications");

    assert_contains(&statement, "tenant_id uuid not null references tenants(id)");
    assert_contains(
        &statement,
        "event_layer_id uuid not null references event_layers(id)",
    );
    assert_contains(&statement, "source_key text not null");
    assert_contains(&statement, "unique (tenant_id, event_layer_id, source_key)");
    assert_contains(
        &sql,
        "alter table event_applications enable row level security",
    );
    assert_contains(&sql, "tenant_isolation_event_applications");
}

#[test]
fn application_capture_has_backup_receipt_outbox() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "event_application_backup_receipts");

    assert_contains(
        &statement,
        "event_application_id uuid not null references event_applications(id)",
    );
    assert_contains(
        &statement,
        "receipt_kind text not null default 'operator_backup_notification'",
    );
    assert_contains(&statement, "payload_json jsonb not null");
    assert_contains(
        &statement,
        "unique (tenant_id, event_application_id, receipt_kind)",
    );
    assert_contains(&sql, "event_application_backup_receipts_pending");
    assert_contains(
        &sql,
        "alter table event_application_backup_receipts enable row level security",
    );
}

#[test]
fn payment_is_not_in_the_intake_schema() {
    let sql = normalized_sql();
    let application_statement = create_table_statement(&sql, "event_applications");

    assert!(!application_statement.contains("stripe"));
    assert!(!application_statement.contains("square"));
    assert!(!application_statement.contains("checkout"));
    assert!(!application_statement.contains("payment_required"));
}
