use std::time::{SystemTime, UNIX_EPOCH};

use rustyred_client::{
    BulkNodeRecord, Client, GeometryContainsRequest, GeometryDesignationRequest, GeometryEncoding,
    GeometryIdsResponse, GeometryQueryRequest,
};
use serde_json::{json, Map, Value};
use sqlx::PgPool;

const NODE_ID: &str = "parcel:postgis-parity";
const PARCEL_WKT: &str = "POLYGON((0 0,4 0,4 4,0 4,0 0))";
const CROSSING_WKT: &str = "LINESTRING(2 -1,2 5)";
const ENVELOPE_WKT: &str = "POLYGON((-1 -1,5 -1,5 5,-1 5,-1 -1))";

#[tokio::test]
#[ignore = "requires reachable DATABASE_URL plus RUSTYRED_URL/RUSTYRED_API_TOKEN for a geometry-enabled RustyRed deployment"]
async fn rustyred_geometry_predicates_match_postgis_reference() {
    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must point at a reachable PostGIS database");
    let pool = PgPool::connect(&database_url)
        .await
        .expect("connect to PostGIS");
    let (postgis_contains, postgis_intersects, postgis_within): (bool, bool, bool) =
        sqlx::query_as(
            r#"
            SELECT
              ST_Contains(
                ST_GeomFromText($1, 4326),
                ST_GeomFromText('POINT(2 2)', 4326)
              ) AS contains_center,
              ST_Intersects(
                ST_GeomFromText($1, 4326),
                ST_GeomFromText($2, 4326)
              ) AS intersects_crossing,
              ST_Within(
                ST_GeomFromText($1, 4326),
                ST_GeomFromText($3, 4326)
              ) AS within_envelope
            "#,
        )
        .bind(PARCEL_WKT)
        .bind(CROSSING_WKT)
        .bind(ENVELOPE_WKT)
        .fetch_one(&pool)
        .await
        .expect("PostGIS predicate reference query");

    let client = Client::from_env().expect("RustyRed client config");
    let tenant_id = unique_tenant_id();

    client
        .designate_geometry(
            &tenant_id,
            &GeometryDesignationRequest {
                label: "Parcel".into(),
                property: "geom".into(),
                encoding: GeometryEncoding::Wkt,
                resolution: 7,
            },
        )
        .await
        .expect("designate geometry index");

    client
        .bulk_nodes(
            &tenant_id,
            &[BulkNodeRecord {
                id: NODE_ID.into(),
                labels: vec!["Parcel".into()],
                properties: Map::from_iter([(
                    "geom".to_string(),
                    Value::String(PARCEL_WKT.into()),
                )]),
            }],
            Some(1),
        )
        .await
        .expect("write parity node");

    let rustyred_contains = has_node(
        client
            .geometry_contains_point(
                &tenant_id,
                &GeometryContainsRequest {
                    label: "Parcel".into(),
                    property: "geom".into(),
                    lat: 2.0,
                    lon: 2.0,
                },
            )
            .await
            .expect("RustyRed contains query"),
    );
    let rustyred_intersects = has_node(
        client
            .geometry_intersects(
                &tenant_id,
                &GeometryQueryRequest {
                    label: "Parcel".into(),
                    property: "geom".into(),
                    geometry: json!(CROSSING_WKT),
                    encoding: GeometryEncoding::Wkt,
                },
            )
            .await
            .expect("RustyRed intersects query"),
    );
    let rustyred_within = has_node(
        client
            .geometry_within(
                &tenant_id,
                &GeometryQueryRequest {
                    label: "Parcel".into(),
                    property: "geom".into(),
                    geometry: json!(ENVELOPE_WKT),
                    encoding: GeometryEncoding::Wkt,
                },
            )
            .await
            .expect("RustyRed within query"),
    );

    assert_eq!(rustyred_contains, postgis_contains);
    assert_eq!(rustyred_intersects, postgis_intersects);
    assert_eq!(rustyred_within, postgis_within);
}

fn has_node(response: GeometryIdsResponse) -> bool {
    response.node_ids.iter().any(|id| id == NODE_ID)
}

fn unique_tenant_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock after unix epoch")
        .as_millis();
    format!("postgis-parity-{millis}")
}
