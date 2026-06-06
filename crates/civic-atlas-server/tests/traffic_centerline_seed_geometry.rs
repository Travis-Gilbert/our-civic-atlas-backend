//! DB-free structural test for traffic centerline seed repair (TR-B2c).

const MIGRATION: &str =
    include_str!("../../../migrations/0020_traffic_centerline_seed_geometry.sql");

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
fn updates_existing_traffic_segments_to_centerline_geometry() {
    let sql = normalized_sql();
    assert_contains(&sql, "update traffic_segments as ts");
    assert_contains(&sql, "st_geomfromgeojson(traced.geojson)::geography");
    assert_contains(&sql, "where ts.network_id = 'flint-downtown'");
    assert_contains(&sql, "geometry updated to centerline-traced seed geometry");
}

#[test]
fn carries_all_six_seed_segment_keys() {
    let sql = normalized_sql();
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
