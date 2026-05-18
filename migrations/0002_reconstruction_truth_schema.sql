CREATE UNIQUE INDEX IF NOT EXISTS parcels_tenant_id_uidx
  ON parcels (tenant_id, id);

CREATE UNIQUE INDEX IF NOT EXISTS buildings_tenant_id_uidx
  ON buildings (tenant_id, id);

CREATE TABLE IF NOT EXISTS building_parts (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  building_id uuid NOT NULL,
  part_key text NOT NULL,
  part_type text NOT NULL CHECK (part_type <> ''),
  geom geometry(Geometry, 4326),
  t_start_ms bigint,
  t_end_ms bigint,
  payload_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  confidence double precision NOT NULL DEFAULT 0 CHECK (confidence >= 0 AND confidence <= 1),
  source_ids text[] NOT NULL DEFAULT '{}'::text[],
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, building_id, part_key),
  FOREIGN KEY (tenant_id, building_id) REFERENCES buildings(tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS artifacts (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  artifact_key text NOT NULL,
  source_type text NOT NULL CHECK (source_type <> ''),
  title text NOT NULL,
  uri text,
  citation text,
  captured_at_ms bigint,
  payload_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, artifact_key)
);

CREATE TABLE IF NOT EXISTS artifact_anchors (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  artifact_id uuid NOT NULL,
  parcel_id uuid,
  building_id uuid,
  building_part_id uuid,
  anchor_kind text NOT NULL DEFAULT 'reference' CHECK (anchor_kind <> ''),
  geom geometry(Geometry, 4326),
  t_start_ms bigint,
  t_end_ms bigint,
  payload_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  CHECK (
    parcel_id IS NOT NULL
    OR building_id IS NOT NULL
    OR building_part_id IS NOT NULL
    OR geom IS NOT NULL
  ),
  FOREIGN KEY (tenant_id, artifact_id) REFERENCES artifacts(tenant_id, id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, parcel_id) REFERENCES parcels(tenant_id, id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, building_id) REFERENCES buildings(tenant_id, id) ON DELETE CASCADE,
  FOREIGN KEY (tenant_id, building_part_id) REFERENCES building_parts(tenant_id, id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS reconstruction_specs (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  spec_id text NOT NULL,
  version integer NOT NULL CHECK (version > 0),
  status text NOT NULL DEFAULT 'draft' CHECK (
    status IN ('draft', 'in_review', 'approved', 'superseded', 'rejected')
  ),
  building_id uuid,
  parcel_id uuid,
  civic_object_id text NOT NULL,
  block_id text,
  title text NOT NULL,
  supersedes_spec_id text,
  supersedes_version integer,
  spec_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_by text NOT NULL DEFAULT '',
  reviewed_by text,
  approved_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, spec_id, version),
  CHECK (
    (supersedes_spec_id IS NULL AND supersedes_version IS NULL)
    OR (supersedes_spec_id IS NOT NULL AND supersedes_version IS NOT NULL)
  ),
  FOREIGN KEY (tenant_id, building_id) REFERENCES buildings(tenant_id, id) ON DELETE RESTRICT,
  FOREIGN KEY (tenant_id, parcel_id) REFERENCES parcels(tenant_id, id) ON DELETE RESTRICT,
  FOREIGN KEY (tenant_id, supersedes_spec_id, supersedes_version)
    REFERENCES reconstruction_specs(tenant_id, spec_id, version) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS generated_assets (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  asset_id text NOT NULL,
  spec_id text NOT NULL,
  spec_version integer NOT NULL CHECK (spec_version > 0),
  asset_type text NOT NULL CHECK (asset_type <> ''),
  uri text NOT NULL,
  content_hash text,
  metadata_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, asset_id),
  FOREIGN KEY (tenant_id, spec_id, spec_version)
    REFERENCES reconstruction_specs(tenant_id, spec_id, version) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS reconstruction_projection_outbox (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  spec_id text NOT NULL,
  spec_version integer NOT NULL CHECK (spec_version > 0),
  projection_kind text NOT NULL CHECK (projection_kind <> ''),
  idempotency_key text NOT NULL,
  payload_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  status text NOT NULL DEFAULT 'pending' CHECK (
    status IN ('pending', 'running', 'succeeded', 'failed')
  ),
  attempt_count integer NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
  next_attempt_at timestamptz,
  last_error text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, idempotency_key),
  FOREIGN KEY (tenant_id, spec_id, spec_version)
    REFERENCES reconstruction_specs(tenant_id, spec_id, version) ON DELETE RESTRICT
);

CREATE TABLE IF NOT EXISTS corrections (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  correction_key text NOT NULL,
  target_type text NOT NULL CHECK (
    target_type IN (
      'parcel',
      'building',
      'building_part',
      'artifact',
      'artifact_anchor',
      'reconstruction_spec',
      'generated_asset'
    )
  ),
  target_id uuid NOT NULL,
  correction_type text NOT NULL CHECK (correction_type <> ''),
  status text NOT NULL DEFAULT 'open' CHECK (
    status IN ('open', 'accepted', 'rejected', 'superseded')
  ),
  payload_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  submitted_by text NOT NULL DEFAULT '',
  reviewed_by text,
  reviewed_at timestamptz,
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  UNIQUE (tenant_id, correction_key)
);

CREATE INDEX IF NOT EXISTS building_parts_tenant_building_idx
  ON building_parts (tenant_id, building_id);
CREATE INDEX IF NOT EXISTS building_parts_confidence_idx
  ON building_parts (tenant_id, confidence);
CREATE INDEX IF NOT EXISTS building_parts_source_ids_gin
  ON building_parts USING gin (source_ids);
CREATE INDEX IF NOT EXISTS building_parts_payload_gin
  ON building_parts USING gin (payload_jsonb);
CREATE INDEX IF NOT EXISTS building_parts_geom_gix
  ON building_parts USING gist (geom);

CREATE INDEX IF NOT EXISTS artifacts_tenant_source_type_idx
  ON artifacts (tenant_id, source_type);
CREATE INDEX IF NOT EXISTS artifacts_payload_gin
  ON artifacts USING gin (payload_jsonb);

CREATE INDEX IF NOT EXISTS artifact_anchors_artifact_idx
  ON artifact_anchors (tenant_id, artifact_id);
CREATE INDEX IF NOT EXISTS artifact_anchors_targets_idx
  ON artifact_anchors (tenant_id, parcel_id, building_id, building_part_id);
CREATE INDEX IF NOT EXISTS artifact_anchors_geom_gix
  ON artifact_anchors USING gist (geom);

CREATE INDEX IF NOT EXISTS reconstruction_specs_status_idx
  ON reconstruction_specs (tenant_id, status);
CREATE INDEX IF NOT EXISTS reconstruction_specs_civic_object_idx
  ON reconstruction_specs (tenant_id, civic_object_id);
CREATE INDEX IF NOT EXISTS reconstruction_specs_building_idx
  ON reconstruction_specs (tenant_id, building_id);
CREATE INDEX IF NOT EXISTS reconstruction_specs_spec_gin
  ON reconstruction_specs USING gin (spec_jsonb);

CREATE INDEX IF NOT EXISTS generated_assets_spec_idx
  ON generated_assets (tenant_id, spec_id, spec_version);
CREATE INDEX IF NOT EXISTS reconstruction_projection_outbox_status_idx
  ON reconstruction_projection_outbox (tenant_id, status, next_attempt_at);
CREATE INDEX IF NOT EXISTS reconstruction_projection_outbox_spec_idx
  ON reconstruction_projection_outbox (tenant_id, spec_id, spec_version);

CREATE INDEX IF NOT EXISTS corrections_target_idx
  ON corrections (tenant_id, target_type, target_id);
CREATE INDEX IF NOT EXISTS corrections_status_idx
  ON corrections (tenant_id, status);
CREATE INDEX IF NOT EXISTS corrections_payload_gin
  ON corrections USING gin (payload_jsonb);

ALTER TABLE building_parts ENABLE ROW LEVEL SECURITY;
ALTER TABLE artifacts ENABLE ROW LEVEL SECURITY;
ALTER TABLE artifact_anchors ENABLE ROW LEVEL SECURITY;
ALTER TABLE reconstruction_specs ENABLE ROW LEVEL SECURITY;
ALTER TABLE generated_assets ENABLE ROW LEVEL SECURITY;
ALTER TABLE reconstruction_projection_outbox ENABLE ROW LEVEL SECURITY;
ALTER TABLE corrections ENABLE ROW LEVEL SECURITY;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'building_parts'
      AND policyname = 'building_parts_current'
  ) THEN
    CREATE POLICY building_parts_current ON building_parts
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'reconstruction_projection_outbox'
      AND policyname = 'reconstruction_projection_outbox_current'
  ) THEN
    CREATE POLICY reconstruction_projection_outbox_current ON reconstruction_projection_outbox
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'artifacts'
      AND policyname = 'artifacts_current'
  ) THEN
    CREATE POLICY artifacts_current ON artifacts
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'artifact_anchors'
      AND policyname = 'artifact_anchors_current'
  ) THEN
    CREATE POLICY artifact_anchors_current ON artifact_anchors
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'reconstruction_specs'
      AND policyname = 'reconstruction_specs_current'
  ) THEN
    CREATE POLICY reconstruction_specs_current ON reconstruction_specs
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'generated_assets'
      AND policyname = 'generated_assets_current'
  ) THEN
    CREATE POLICY generated_assets_current ON generated_assets
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'corrections'
      AND policyname = 'corrections_current'
  ) THEN
    CREATE POLICY corrections_current ON corrections
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

CREATE OR REPLACE FUNCTION prevent_approved_reconstruction_spec_mutation()
RETURNS trigger AS $$
BEGIN
  IF OLD.status = 'approved' THEN
    RAISE EXCEPTION 'approved reconstruction_specs are immutable';
  END IF;

  IF TG_OP = 'DELETE' THEN
    RETURN OLD;
  END IF;

  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS reconstruction_specs_approved_immutable_update
  ON reconstruction_specs;
CREATE TRIGGER reconstruction_specs_approved_immutable_update
  BEFORE UPDATE ON reconstruction_specs
  FOR EACH ROW
  EXECUTE FUNCTION prevent_approved_reconstruction_spec_mutation();

DROP TRIGGER IF EXISTS reconstruction_specs_approved_immutable_delete
  ON reconstruction_specs;
CREATE TRIGGER reconstruction_specs_approved_immutable_delete
  BEFORE DELETE ON reconstruction_specs
  FOR EACH ROW
  EXECUTE FUNCTION prevent_approved_reconstruction_spec_mutation();
