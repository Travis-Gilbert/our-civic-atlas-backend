-- Seed data for the Carriage Town Phase 4 PostGIS live gate.
--
-- Hand-encoded 5 reconstruction specs against fictional buildings in
-- the Carriage Town neighborhood of Flint. Coordinates are clustered
-- near the real neighborhood at roughly 43.012N, -83.700W. The geometry
-- is approximate; this seed exists to exercise the schema and the
-- query paths (GetBlockSubgraph, ListReconstructionSpecs, the public
-- /changelog feed), not as historical record. Real historical sources
-- get plumbed through Sanborn / HABS ingestion in Phase 5.
--
-- The migration is idempotent: every INSERT is gated by an existence
-- check on (tenant_id, *_key) so re-running is a no-op.

-- Helper function: seed one reconstruction spec + project its parts
-- to the building_parts table (mirrors what civic-atlas-server's
-- approve_spec does). Idempotent on (tenant, spec_id, version).
CREATE OR REPLACE FUNCTION seed_carriage_town_spec(
  in_tenant_id uuid,
  in_block_id text,
  in_civic_object_id text,
  in_spec_id text,
  in_title text,
  in_mass_form text,
  in_facade_material text,
  in_facade_color text,
  in_story_count integer,
  in_roof_form text,
  in_roof_material text,
  in_confidence double precision
)
RETURNS void AS $func$
DECLARE
  building_uuid uuid;
  spec_jsonb_value jsonb;
BEGIN
  SELECT id INTO building_uuid FROM buildings
    WHERE tenant_id = in_tenant_id AND civic_object_id = in_civic_object_id;
  IF building_uuid IS NULL THEN
    RAISE NOTICE 'building % missing; skipping spec seed', in_civic_object_id;
    RETURN;
  END IF;

  spec_jsonb_value := jsonb_build_object(
    'tenant_context', jsonb_build_object('tenant_id', 'flint'),
    'spec_id', in_spec_id,
    'civic_object_id', in_civic_object_id,
    'building_id', building_uuid::text,
    'block_id', in_block_id,
    'title', in_title,
    'status', 'approved',
    'version', 1,
    'mass', jsonb_build_object(
      'provenance', jsonb_build_object(
        'sources', '[]'::jsonb,
        'confidence', in_confidence,
        'from_gnn_prior', false,
        'reviewer_note', 'hand-encoded seed'
      ),
      'form', in_mass_form,
      'story_count', in_story_count
    ),
    'facades', jsonb_build_array(
      jsonb_build_object(
        'provenance', jsonb_build_object(
          'sources', '[]'::jsonb,
          'confidence', in_confidence,
          'from_gnn_prior', false
        ),
        'orientation', 'south',
        'material', in_facade_material,
        'color', in_facade_color
      )
    ),
    'roof', jsonb_build_object(
      'provenance', jsonb_build_object(
        'sources', '[]'::jsonb,
        'confidence', in_confidence,
        'from_gnn_prior', false
      ),
      'form', in_roof_form,
      'material', in_roof_material
    ),
    'ground_floor', jsonb_build_object(
      'provenance', jsonb_build_object(
        'sources', '[]'::jsonb,
        'confidence', in_confidence * 0.9,
        'from_gnn_prior', false
      ),
      'use_type', 'residential',
      'entry_location', 'south'
    )
  );

  INSERT INTO reconstruction_specs (
    tenant_id, spec_id, version, status,
    building_id, civic_object_id, block_id, title,
    spec_jsonb, created_by, reviewed_by, approved_at
  ) VALUES (
    in_tenant_id, in_spec_id, 1, 'approved',
    building_uuid, in_civic_object_id, in_block_id, in_title,
    spec_jsonb_value, 'seed:0004', 'seed:0004', now()
  )
  ON CONFLICT (tenant_id, spec_id, version) DO NOTHING;

  -- Project the parts (idempotent).
  INSERT INTO building_parts (
    tenant_id, building_id, part_key, part_type, payload_jsonb, confidence, source_ids
  ) VALUES
    (in_tenant_id, building_uuid, 'mass', 'mass',
     spec_jsonb_value -> 'mass', in_confidence, '{}'::text[]),
    (in_tenant_id, building_uuid, 'facade::south', 'facade',
     spec_jsonb_value -> 'facades' -> 0, in_confidence, '{}'::text[]),
    (in_tenant_id, building_uuid, 'roof', 'roof',
     spec_jsonb_value -> 'roof', in_confidence, '{}'::text[]),
    (in_tenant_id, building_uuid, 'ground_floor', 'ground_floor',
     spec_jsonb_value -> 'ground_floor', in_confidence * 0.9, '{}'::text[])
  ON CONFLICT (tenant_id, building_id, part_key) DO NOTHING;

  -- Enqueue a projection outbox row so the outbox worker picks it
  -- up. With THESEUS_BRIDGE_URL unset the worker logs the projection
  -- and marks succeeded; with the URL set, a real RustyRed projection
  -- happens once the bridge RPC ships.
  INSERT INTO reconstruction_projection_outbox (
    tenant_id, spec_id, spec_version, projection_kind,
    idempotency_key, payload_jsonb, status
  ) VALUES (
    in_tenant_id, in_spec_id, 1, 'BuildingPresence',
    'seed::' || in_spec_id || '::v1',
    jsonb_build_object(
      'projectionKind', 'BuildingPresence',
      'specId', in_spec_id,
      'version', 1,
      'buildingId', building_uuid::text,
      'civicObjectId', in_civic_object_id
    ),
    'pending'
  )
  ON CONFLICT (tenant_id, idempotency_key) DO NOTHING;
END
$func$ LANGUAGE plpgsql;

DO $$
DECLARE
  flint_tenant_id uuid;
  block_id_value text := 'block:carriage-town:central';
BEGIN
  -- 1. Tenant
  INSERT INTO tenants (slug, display_name)
  VALUES ('flint', 'Flint')
  ON CONFLICT (slug) DO NOTHING;

  SELECT id INTO flint_tenant_id FROM tenants WHERE slug = 'flint';
  IF flint_tenant_id IS NULL THEN
    RAISE EXCEPTION 'flint tenant could not be resolved';
  END IF;

  INSERT INTO tenant_runtime_namespaces (tenant_id, rustyred_namespace)
  VALUES (flint_tenant_id, 'flint')
  ON CONFLICT (tenant_id) DO NOTHING;

  PERFORM set_config('app.tenant_id', flint_tenant_id::text, true);

  -- 2. Parcels (5)
  INSERT INTO parcels (tenant_id, parcel_key, geom, properties)
  VALUES
    (flint_tenant_id, 'carriage-town:1',
     ST_GeomFromText('MULTIPOLYGON(((-83.7005 43.0125, -83.7000 43.0125, -83.7000 43.0130, -83.7005 43.0130, -83.7005 43.0125)))', 4326),
     jsonb_build_object('block_id', block_id_value, 'address', '624 E Kearsley St')),
    (flint_tenant_id, 'carriage-town:2',
     ST_GeomFromText('MULTIPOLYGON(((-83.7000 43.0125, -83.6995 43.0125, -83.6995 43.0130, -83.7000 43.0130, -83.7000 43.0125)))', 4326),
     jsonb_build_object('block_id', block_id_value, 'address', '628 E Kearsley St')),
    (flint_tenant_id, 'carriage-town:3',
     ST_GeomFromText('MULTIPOLYGON(((-83.6995 43.0125, -83.6990 43.0125, -83.6990 43.0130, -83.6995 43.0130, -83.6995 43.0125)))', 4326),
     jsonb_build_object('block_id', block_id_value, 'address', '632 E Kearsley St')),
    (flint_tenant_id, 'carriage-town:4',
     ST_GeomFromText('MULTIPOLYGON(((-83.7005 43.0120, -83.7000 43.0120, -83.7000 43.0125, -83.7005 43.0125, -83.7005 43.0120)))', 4326),
     jsonb_build_object('block_id', block_id_value, 'address', '625 E Kearsley St')),
    (flint_tenant_id, 'carriage-town:5',
     ST_GeomFromText('MULTIPOLYGON(((-83.7000 43.0120, -83.6995 43.0120, -83.6995 43.0125, -83.7000 43.0125, -83.7000 43.0120)))', 4326),
     jsonb_build_object('block_id', block_id_value, 'address', '629 E Kearsley St'))
  ON CONFLICT (tenant_id, parcel_key) DO NOTHING;

  -- 3. Buildings (5)
  INSERT INTO buildings (tenant_id, parcel_id, civic_object_id, geom, t_start_ms, t_end_ms, properties)
  SELECT
    flint_tenant_id,
    p.id,
    'building:' || p.parcel_key,
    ST_Multi(ST_Buffer(ST_Centroid(p.geom)::geography, 8)::geometry)::geometry(MultiPolygon, 4326),
    EXTRACT(EPOCH FROM TIMESTAMP '1885-01-01 00:00:00Z')::bigint * 1000,
    NULL,
    jsonb_build_object('block_id', block_id_value)
  FROM parcels p
  WHERE p.tenant_id = flint_tenant_id
    AND p.parcel_key IN (
      'carriage-town:1','carriage-town:2','carriage-town:3',
      'carriage-town:4','carriage-town:5'
    )
  ON CONFLICT (tenant_id, civic_object_id) DO NOTHING;

  -- 4. Reconstruction specs (5, all approved)
  PERFORM seed_carriage_town_spec(
    flint_tenant_id, block_id_value,
    'building:carriage-town:1', 'spec:carriage-town:1',
    'Whaley House (1885)',
    'italianate brick mansion', 'brick', 'red', 3,
    'hipped', 'slate', 0.92
  );
  PERFORM seed_carriage_town_spec(
    flint_tenant_id, block_id_value,
    'building:carriage-town:2', 'spec:carriage-town:2',
    '628 E Kearsley Frame House',
    'wood frame queen anne', 'wood', 'cream', 2,
    'gable', 'asphalt shingle', 0.78
  );
  PERFORM seed_carriage_town_spec(
    flint_tenant_id, block_id_value,
    'building:carriage-town:3', 'spec:carriage-town:3',
    'Carriage Town Storefront',
    'brick commercial main-street', 'brick', 'tan', 2,
    'flat', 'tar and gravel', 0.65
  );
  PERFORM seed_carriage_town_spec(
    flint_tenant_id, block_id_value,
    'building:carriage-town:4', 'spec:carriage-town:4',
    'Worker''s Cottage (1898)',
    'wood frame cottage', 'wood', 'olive', 1,
    'gable', 'wood shingle', 0.55
  );
  PERFORM seed_carriage_town_spec(
    flint_tenant_id, block_id_value,
    'building:carriage-town:5', 'spec:carriage-town:5',
    'Stockton House (1872)',
    'greek revival timber', 'wood', 'white', 2,
    'gable', 'wood shingle', 0.71
  );

  -- 5. Three artifacts + their anchors
  INSERT INTO artifacts (tenant_id, artifact_key, source_type, title, uri, citation)
  VALUES
    (flint_tenant_id, 'artifact:whaley-photo-1908', 'archival_photo',
     'Whaley House photograph, 1908',
     'https://example.org/loc/habs/whaley-1908.jpg',
     'HABS MI-318 / Library of Congress'),
    (flint_tenant_id, 'artifact:carriage-sanborn-1899', 'map',
     'Sanborn fire insurance map, Flint 1899 sheet 18',
     'https://example.org/loc/sanborn/flint-1899-s18.tif',
     'Sanborn 1899, Library of Congress'),
    (flint_tenant_id, 'artifact:storefront-photo-1925', 'archival_photo',
     'E Kearsley storefront circa 1925',
     'https://example.org/sloan/storefront-1925.jpg',
     'Sloan Museum of Discovery')
  ON CONFLICT (tenant_id, artifact_key) DO NOTHING;

  INSERT INTO artifact_anchors (tenant_id, artifact_id, building_id, anchor_kind)
  SELECT flint_tenant_id, a.id, b.id, 'photographic'
  FROM artifacts a, buildings b
  WHERE a.tenant_id = flint_tenant_id
    AND b.tenant_id = flint_tenant_id
    AND a.artifact_key = 'artifact:whaley-photo-1908'
    AND b.civic_object_id = 'building:carriage-town:1'
  ON CONFLICT DO NOTHING;

  INSERT INTO artifact_anchors (tenant_id, artifact_id, building_id, anchor_kind)
  SELECT flint_tenant_id, a.id, b.id, 'photographic'
  FROM artifacts a, buildings b
  WHERE a.tenant_id = flint_tenant_id
    AND b.tenant_id = flint_tenant_id
    AND a.artifact_key = 'artifact:storefront-photo-1925'
    AND b.civic_object_id = 'building:carriage-town:3'
  ON CONFLICT DO NOTHING;

  INSERT INTO artifact_anchors (tenant_id, artifact_id, building_id, anchor_kind)
  SELECT flint_tenant_id, a.id, b.id, 'cartographic'
  FROM artifacts a, buildings b
  WHERE a.tenant_id = flint_tenant_id
    AND b.tenant_id = flint_tenant_id
    AND a.artifact_key = 'artifact:carriage-sanborn-1899'
    AND b.civic_object_id LIKE 'building:carriage-town:%'
  ON CONFLICT DO NOTHING;
END
$$;
