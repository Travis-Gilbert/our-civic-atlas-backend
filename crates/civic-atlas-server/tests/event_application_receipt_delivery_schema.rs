const MIGRATION: &str =
    include_str!("../../../migrations/0025_event_application_receipt_delivery.sql");

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
fn application_receipts_are_retryable() {
    let sql = normalized_sql();

    assert_contains(
        &sql,
        "alter table event_application_backup_receipts add column attempt_count integer not null default 0",
    );
    assert_contains(&sql, "add column last_error text");
    assert_contains(&sql, "add column next_attempt_at timestamptz");
    assert_contains(&sql, "idx_event_application_backup_receipts_retry");
    assert_contains(&sql, "where status in ('pending', 'running')");
}
