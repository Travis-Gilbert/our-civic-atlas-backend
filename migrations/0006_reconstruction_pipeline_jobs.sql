CREATE TABLE IF NOT EXISTS reconstruction_jobs (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  parcel_id text NOT NULL CHECK (parcel_id <> ''),
  time_slice_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  requested_by text NOT NULL DEFAULT '',
  auto_approve boolean NOT NULL DEFAULT false,
  status text NOT NULL DEFAULT 'pending' CHECK (
    status IN ('pending', 'running', 'succeeded', 'failed')
  ),
  attempt_count integer NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
  next_attempt_at timestamptz,
  resulting_spec_id text,
  resulting_spec_version integer,
  stage_report_jsonb jsonb NOT NULL DEFAULT '{}'::jsonb,
  last_error text,
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id)
);

CREATE INDEX IF NOT EXISTS reconstruction_jobs_status_idx
  ON reconstruction_jobs (tenant_id, status, next_attempt_at);
CREATE INDEX IF NOT EXISTS reconstruction_jobs_parcel_idx
  ON reconstruction_jobs (tenant_id, parcel_id);

ALTER TABLE reconstruction_jobs ENABLE ROW LEVEL SECURITY;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'reconstruction_jobs'
      AND policyname = 'reconstruction_jobs_current'
  ) THEN
    CREATE POLICY reconstruction_jobs_current ON reconstruction_jobs
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;
