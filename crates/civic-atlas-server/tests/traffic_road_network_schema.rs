//! DB-free structural test for the traffic road-network migration (TR-B2).
//!
//! Asserts the migration SQL shape without a live Postgres, mirroring the other
//! migration tests (e.g. zoning_envelope_schema.rs). The migration is ALSO
//! validated against a real PostGIS container before shipping (it applies on top
//! of 0001..0018 and seeds 6 valid LineString corridors); this test guards the
//! shape in CI so a future edit can't silently drop a column, the RLS policy, or
//! the FK-safe seed.

const MIGRATION: &str = include_str!("../../../migrations/0019_traffic_road_network.sql");

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
fn creates_traffic_segments_table_with_rls() {
    let sql = normalized_sql();
    assert_contains(&sql, "create table if not exists traffic_segments (");
    assert_contains(
        &sql,
        "tenant_id uuid not null references tenants(id) on delete cascade",
    );
    assert_contains(&sql, "geometry geography(linestring, 4326) not null");
    assert_contains(&sql, "unique (tenant_id, network_id, segment_key)");
    assert_contains(
        &sql,
        "alter table traffic_segments enable row level security",
    );
    assert_contains(
        &sql,
        "create policy tenant_isolation_traffic_segments on traffic_segments",
    );
    assert_contains(&sql, "using gist (geometry)");
}

#[test]
fn carries_contract_columns() {
    let sql = normalized_sql();
    for col in [
        "network_id text not null",
        "segment_key text not null",
        "corridor_name text not null",
        "direction_label text not null",
        "estimate_basis text not null",
        "source_status text not null",
        "source_label text not null",
        "support_note text not null",
        "free_flow_speed_mph double precision not null",
        "base_speed_mph double precision not null",
        "base_volume_per_hour double precision not null",
        "confidence double precision not null",
    ] {
        assert_contains(&sql, col);
    }
}

#[test]
fn seeds_flint_corridors_fk_safely() {
    let sql = normalized_sql();
    // The seed is gated on the flint tenant (FK-safe, zero rows on a fresh DB)
    // and idempotent.
    assert_contains(&sql, "from tenants t");
    assert_contains(&sql, "where t.slug = 'flint'");
    assert_contains(
        &sql,
        "on conflict (tenant_id, network_id, segment_key) do nothing",
    );
    assert_contains(&sql, "st_geomfromgeojson(v.geojson)::geography");
    for key in [
        "traffic:flint:i-69:west",
        "traffic:flint:i-475:spine",
        "traffic:flint:court:midtown",
        "traffic:flint:saginaw:downtown",
        "traffic:flint:dort:east",
        "traffic:flint:miller:southwest",
    ] {
        assert_contains(&sql, key);
    }
}
