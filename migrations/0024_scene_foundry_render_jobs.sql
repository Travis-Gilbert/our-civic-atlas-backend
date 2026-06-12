-- Scene Foundry GPU refinement jobs.
--
-- The synchronous renderer (civic-atlas-renderer) writes the massing GLB
-- inside generate_assets and enqueues one row here per (spec, tier kind).
-- The outbox worker claims rows with FOR UPDATE SKIP LOCKED, calls the Ray
-- Serve renderer app in civic-atlas-ingest (SCENE_FOUNDRY_RENDER_URL), and
-- upserts the returned refined assets into generated_assets.

CREATE TABLE IF NOT EXISTS scene_foundry_render_jobs (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  spec_id text NOT NULL CHECK (spec_id <> ''),
  spec_version integer NOT NULL CHECK (spec_version >= 0),
  render_tier text NOT NULL CHECK (render_tier <> ''),
  job_kind text NOT NULL CHECK (
    job_kind IN (
      'single_facade_fit',
      'sparse_multiview',
      'gaussian_splatting',
      'procedural_archetype'
    )
  ),
  status text NOT NULL DEFAULT 'pending' CHECK (
    status IN ('pending', 'running', 'succeeded', 'failed')
  ),
  spec_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  photo_sources_jsonb jsonb NOT NULL DEFAULT '[]'::jsonb,
  result_assets_jsonb jsonb NOT NULL DEFAULT '[]'::jsonb,
  attempt_count integer NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
  next_attempt_at timestamptz,
  last_error text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, spec_id, spec_version, job_kind)
);

CREATE INDEX IF NOT EXISTS scene_foundry_render_jobs_status_idx
  ON scene_foundry_render_jobs (tenant_id, status, next_attempt_at);
CREATE INDEX IF NOT EXISTS scene_foundry_render_jobs_spec_idx
  ON scene_foundry_render_jobs (tenant_id, spec_id, spec_version);

ALTER TABLE scene_foundry_render_jobs ENABLE ROW LEVEL SECURITY;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'scene_foundry_render_jobs'
      AND policyname = 'scene_foundry_render_jobs_current'
  ) THEN
    CREATE POLICY scene_foundry_render_jobs_current ON scene_foundry_render_jobs
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;
