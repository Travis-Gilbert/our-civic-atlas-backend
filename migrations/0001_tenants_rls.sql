CREATE EXTENSION IF NOT EXISTS postgis;
CREATE EXTENSION IF NOT EXISTS pgcrypto;

CREATE TABLE IF NOT EXISTS tenants (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  slug text NOT NULL UNIQUE CHECK (slug ~ '^[a-z0-9-]+$'),
  display_name text NOT NULL,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS tenant_runtime_namespaces (
  tenant_id uuid PRIMARY KEY REFERENCES tenants(id) ON DELETE CASCADE,
  rustyred_namespace text NOT NULL UNIQUE,
  created_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS parcels (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  parcel_key text NOT NULL,
  geom geometry(MultiPolygon, 4326) NOT NULL,
  properties jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, parcel_key)
);

CREATE TABLE IF NOT EXISTS buildings (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  parcel_id uuid REFERENCES parcels(id) ON DELETE SET NULL,
  civic_object_id text NOT NULL,
  geom geometry(MultiPolygon, 4326),
  t_start_ms bigint,
  t_end_ms bigint,
  properties jsonb NOT NULL DEFAULT '{}'::jsonb,
  created_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, civic_object_id)
);

ALTER TABLE tenants ENABLE ROW LEVEL SECURITY;
ALTER TABLE tenant_runtime_namespaces ENABLE ROW LEVEL SECURITY;
ALTER TABLE parcels ENABLE ROW LEVEL SECURITY;
ALTER TABLE buildings ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenants_current ON tenants
  USING (id::text = current_setting('app.tenant_id', true));

CREATE POLICY tenant_runtime_namespaces_current ON tenant_runtime_namespaces
  USING (tenant_id::text = current_setting('app.tenant_id', true));

CREATE POLICY parcels_current ON parcels
  USING (tenant_id::text = current_setting('app.tenant_id', true))
  WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

CREATE POLICY buildings_current ON buildings
  USING (tenant_id::text = current_setting('app.tenant_id', true))
  WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

CREATE INDEX IF NOT EXISTS parcels_geom_gix ON parcels USING gist (geom);
CREATE INDEX IF NOT EXISTS buildings_geom_gix ON buildings USING gist (geom);
CREATE INDEX IF NOT EXISTS buildings_tenant_time_idx
  ON buildings (tenant_id, t_start_ms, t_end_ms);

