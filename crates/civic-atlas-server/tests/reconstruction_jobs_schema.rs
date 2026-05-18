const MIGRATION: &str = include_str!("../../../migrations/0006_reconstruction_pipeline_jobs.sql");

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
fn reconstruction_jobs_are_tenant_scoped_and_retryable() {
    let sql = normalized_sql();
    let statement = create_table_statement(&sql, "reconstruction_jobs");

    assert_contains(&statement, "tenant_id uuid not null");
    assert_contains(
        &statement,
        "parcel_id text not null check (parcel_id <> '')",
    );
    assert_contains(
        &statement,
        "time_slice_jsonb jsonb not null default '{}'::jsonb",
    );
    assert_contains(
        &statement,
        "status text not null default 'pending' check ( status in ('pending', 'running', 'succeeded', 'failed') )",
    );
    assert_contains(&statement, "attempt_count integer not null default 0");
    assert_contains(&statement, "next_attempt_at timestamptz");
    assert_contains(
        &sql,
        "reconstruction_jobs_status_idx on reconstruction_jobs (tenant_id, status, next_attempt_at)",
    );
}

#[test]
fn reconstruction_jobs_have_current_tenant_policy() {
    let sql = normalized_sql();

    assert_contains(
        &sql,
        "alter table reconstruction_jobs enable row level security",
    );
    assert_contains(
        &sql,
        "create policy reconstruction_jobs_current on reconstruction_jobs using (tenant_id::text = current_setting('app.tenant_id', true)) with check (tenant_id::text = current_setting('app.tenant_id', true))",
    );
}
