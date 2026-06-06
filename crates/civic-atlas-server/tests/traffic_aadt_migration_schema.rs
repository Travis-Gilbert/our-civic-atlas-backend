//! DB-free structural test for the real-AADT Flint seed migration (TR: real data).
//!
//! Asserts the migration shape + honesty invariants without a live Postgres
//! (mirrors traffic_road_network_schema.rs). The migration is generated from
//! MDOT 2024 AADT (scripts/build_flint_aadt_migration.py) and is ALSO validated
//! against a real PostGIS container before shipping (applies on top of
//! 0001..0020, DELETEs the placeholders, seeds 60 real LineString segments).

const MIGRATION: &str = include_str!("../../../migrations/0021_flint_aadt_segments.sql");

fn normalized_sql() -> String {
    MIGRATION
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[test]
fn replaces_placeholders_with_real_aadt() {
    let sql = normalized_sql();
    // Replace the placeholder corridors with the real seed.
    assert!(sql.contains("delete from traffic_segments"));
    assert!(sql.contains("insert into traffic_segments"));
    assert!(sql.contains("st_geomfromgeojson("));
    // FK-safe, idempotent, tenant-gated (same pattern as 0019).
    assert!(sql.contains("from tenants t"));
    assert!(sql.contains("where t.slug = 'flint'"));
    assert!(sql.contains("on conflict (tenant_id, network_id, segment_key) do nothing"));
}

#[test]
fn provenance_is_measured_historic_not_live() {
    let sql = normalized_sql();
    // Honest provenance: real MDOT 2024 AADT, an hourly-pattern historic average.
    assert!(sql.contains("mdot 2024 aadt"));
    assert!(sql.contains("'hourly_pattern'"));
    assert!(sql.contains("'historic_average'"));
    // A historic seed never claims a live source_status.
    assert!(
        !sql.contains("'live'"),
        "AADT historic seed must not mark any segment source_status='live'"
    );
}

#[test]
fn carries_a_meaningful_number_of_real_segments() {
    // One ST_GeomFromGeoJSON per seeded segment; the top-by-AADT Flint set.
    let count = MIGRATION.matches("ST_GeomFromGeoJSON").count();
    assert!(
        count >= 50,
        "expected >= 50 real AADT segments, found {count}"
    );
}
