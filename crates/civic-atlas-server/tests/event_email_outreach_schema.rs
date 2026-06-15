const MIGRATION: &str = include_str!("../../../migrations/0026_event_email_outreach.sql");

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
fn email_channel_is_event_scoped_and_secret_free() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "event_email_channels");

    assert_contains(&statement, "tenant_id uuid not null references tenants(id)");
    assert_contains(
        &statement,
        "event_layer_id uuid not null references event_layers(id)",
    );
    assert_contains(&statement, "provider text not null default 'resend'");
    assert_contains(&statement, "sender_email text not null");
    assert_contains(&statement, "delivery_webhook_status text not null");
    assert_contains(&statement, "unique (tenant_id, event_layer_id)");
    assert!(
        !statement.contains("api_key") && !statement.contains("webhook_secret"),
        "email channel must not persist provider secrets"
    );
}

#[test]
fn outreach_is_idempotent_and_provider_linkable() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "event_email_outreach");

    assert_contains(
        &statement,
        "application_id uuid references event_applications(id)",
    );
    assert_contains(&statement, "recipient_email text not null");
    assert_contains(&statement, "resend_email_id text");
    assert_contains(&statement, "idempotency_key text");
    assert_contains(
        &sql,
        "create unique index idx_event_email_outreach_idempotency",
    );
    assert_contains(
        &sql,
        "on event_email_outreach (tenant_id, event_layer_id, idempotency_key)",
    );
    assert_contains(
        &sql,
        "create unique index idx_event_email_outreach_resend_email",
    );
}

#[test]
fn provider_events_are_replay_safe() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "event_email_events");

    assert_contains(&statement, "provider_event_id text not null");
    assert_contains(&statement, "payload_json jsonb not null");
    assert_contains(
        &statement,
        "unique (tenant_id, provider, provider_event_id)",
    );
    assert_contains(
        &statement,
        "outreach_id uuid references event_email_outreach(id)",
    );
    assert_contains(
        &sql,
        "alter table event_email_events enable row level security",
    );
    assert_contains(&sql, "tenant_isolation_event_email_events");
}
