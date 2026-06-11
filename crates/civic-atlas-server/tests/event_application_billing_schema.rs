const MIGRATION: &str = include_str!("../../../migrations/0023_event_application_billing.sql");

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
fn billing_requests_are_tenant_scoped_and_idempotent() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "event_application_billing_requests");

    assert_contains(&statement, "tenant_id uuid not null references tenants(id)");
    assert_contains(
        &statement,
        "event_application_id uuid not null references event_applications(id)",
    );
    assert_contains(&statement, "provider text not null default 'square'");
    assert_contains(&statement, "idempotency_key text not null");
    assert_contains(
        &statement,
        "unique (tenant_id, event_application_id, provider, idempotency_key)",
    );
    assert_contains(
        &sql,
        "alter table event_application_billing_requests enable row level security",
    );
    assert_contains(&sql, "tenant_isolation_event_application_billing_requests");
}

#[test]
fn billing_requests_store_square_link_references() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "event_application_billing_requests");

    assert_contains(
        &statement,
        "amount_cents bigint not null check (amount_cents > 0)",
    );
    assert_contains(&statement, "currency text not null default 'usd'");
    assert_contains(&statement, "payment_link_url text");
    assert_contains(&statement, "provider_payment_link_id text");
    assert_contains(&statement, "provider_order_id text");
    assert_contains(&statement, "request_payload_json jsonb not null");
    assert_contains(&statement, "response_payload_json jsonb not null");
}
