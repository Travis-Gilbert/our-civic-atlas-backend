-- Traffic Domain TR-B2: the road-network segment table the realtime traffic
-- resolver reads (schema Extension 8, trafficRealtime). Until the live MDOT RIDE
-- feed (TR-B3) writes measured rows, this holds the seed Flint corridors that
-- mirror the honest fixture `trafficRealtime` already returns. Cross-repo plan:
-- docs/plans/traffic-domain-realtime/ in Open-Flint-Atlas-main-release.
--
-- Mirrors migration 0011 conventions: tenant_id uuid REFERENCES tenants(id),
-- RLS via the app.tenant_id GUC, PostGIS geography for WGS84-correct geometry,
-- gist index. Stores reference values (free-flow ceiling + peak-congested base);
-- the resolver applies the diurnal curve, so an empty/seeded table both behave.

CREATE TABLE IF NOT EXISTS traffic_segments (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    network_id text NOT NULL,
    segment_key text NOT NULL,
    corridor_name text NOT NULL,
    direction_label text NOT NULL,
    geometry geography(LINESTRING, 4326) NOT NULL,
    estimate_basis text NOT NULL DEFAULT 'hourly_pattern',
    source_status text NOT NULL DEFAULT 'fixture',
    source_label text NOT NULL,
    support_note text NOT NULL,
    free_flow_speed_mph double precision NOT NULL,
    base_speed_mph double precision NOT NULL,
    base_volume_per_hour double precision NOT NULL,
    confidence double precision NOT NULL DEFAULT 0.5,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, network_id, segment_key)
);

CREATE INDEX IF NOT EXISTS idx_traffic_segments_network
    ON traffic_segments (tenant_id, network_id);
CREATE INDEX IF NOT EXISTS idx_traffic_segments_geom
    ON traffic_segments USING gist (geometry);

ALTER TABLE traffic_segments ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation_traffic_segments ON traffic_segments
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

-- Seed the Flint corridors for the 'flint' tenant. FK-safe: the SELECT yields
-- zero rows when the flint tenant does not exist yet (fresh DB), so this never
-- violates the FK. Idempotent via ON CONFLICT. geometry from GeoJSON via
-- ST_GeomFromGeoJSON (GeoJSON is WGS84) cast to geography.
INSERT INTO traffic_segments (
    tenant_id, network_id, segment_key, corridor_name, direction_label,
    geometry, estimate_basis, source_status, source_label, support_note,
    free_flow_speed_mph, base_speed_mph, base_volume_per_hour, confidence
)
SELECT
    t.id,
    'flint-downtown',
    v.segment_key,
    v.corridor_name,
    v.direction_label,
    ST_GeomFromGeoJSON(v.geojson)::geography,
    v.estimate_basis,
    'fixture',
    v.source_label,
    v.support_note,
    v.free_flow_speed_mph,
    v.base_speed_mph,
    v.base_volume_per_hour,
    v.confidence
FROM tenants t
CROSS JOIN (
    VALUES
        ('traffic:flint:i-69:west', 'I-69 west approach', 'Eastbound / west-side approach',
         '{"type":"LineString","coordinates":[[-83.7514,42.9989],[-83.7347,42.9992],[-83.7116,42.9995],[-83.6904,42.9991]]}',
         'live_feed', 'MDOT RIDE target, fixture mirror',
         'Fixture segment shaped to the public RIDE handoff contract until the authenticated live feed is wired.',
         65.0::double precision, 56.0::double precision, 1280.0::double precision, 0.82::double precision),
        ('traffic:flint:i-475:spine', 'I-475 city spine', 'North / south trunkline',
         '{"type":"LineString","coordinates":[[-83.7047,43.0476],[-83.7041,43.0315],[-83.7016,43.0122],[-83.6991,42.9918]]}',
         'live_feed', 'MDOT RIDE target, fixture mirror',
         'Represents the first trunkline corridor a live loop-detector feed should map onto.',
         60.0, 49.0, 1640.0, 0.84),
        ('traffic:flint:court:midtown', 'Court Street / M-21', 'Downtown east-west corridor',
         '{"type":"LineString","coordinates":[[-83.7346,43.012],[-83.7168,43.0121],[-83.6978,43.0124],[-83.6798,43.0125]]}',
         'hourly_pattern', 'Hourly pattern seed',
         'Local arterial sample inferred from an hourly traffic pattern until current counts are available.',
         35.0, 27.0, 760.0, 0.62),
        ('traffic:flint:saginaw:downtown', 'Saginaw Street downtown', 'Downtown north-south street',
         '{"type":"LineString","coordinates":[[-83.6938,43.0308],[-83.694,43.0218],[-83.6933,43.0142],[-83.6925,43.0046]]}',
         'hourly_pattern', 'Hourly pattern seed',
         'Downtown street sample, useful for proving density-and-speed rendering before signal timing feeds are connected.',
         28.0, 18.0, 540.0, 0.58),
        ('traffic:flint:dort:east', 'Dort Highway', 'East-side north-south corridor',
         '{"type":"LineString","coordinates":[[-83.6558,43.0412],[-83.6557,43.0228],[-83.6552,43.0004],[-83.6547,42.981]]}',
         'live_feed', 'MDOT RIDE target, fixture mirror',
         'High-volume east-side corridor sample for live-feed segment mapping.',
         50.0, 42.0, 1120.0, 0.78),
        ('traffic:flint:miller:southwest', 'Miller Road', 'Southwest commercial corridor',
         '{"type":"LineString","coordinates":[[-83.7835,42.9901],[-83.7612,42.9904],[-83.7418,42.9906],[-83.7228,42.9908]]}',
         'hourly_pattern', 'Hourly pattern seed',
         'Commercial-corridor sample for inferred demand and later count calibration.',
         40.0, 31.0, 880.0, 0.6)
) AS v(
    segment_key, corridor_name, direction_label, geojson, estimate_basis,
    source_label, support_note, free_flow_speed_mph, base_speed_mph,
    base_volume_per_hour, confidence
)
WHERE t.slug = 'flint'
ON CONFLICT (tenant_id, network_id, segment_key) DO NOTHING;
