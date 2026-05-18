-- Phase 4 community correction loop extensions.
--
-- Extends the polymorphic `corrections` table from migration 0002
-- with submitter IP hashing (for rate limiting + audit), moderator
-- notes, per-part accept selections, and resulting spec linkage.
-- Adds new tables for rate limiting anonymous submissions and for
-- the public changelog at /open-flint-atlas/changelog.

-- 1. Extend corrections with Phase 4 fields.

ALTER TABLE corrections
  ADD COLUMN IF NOT EXISTS submitter_ip_hash text;

ALTER TABLE corrections
  ADD COLUMN IF NOT EXISTS moderator_notes text;

ALTER TABLE corrections
  ADD COLUMN IF NOT EXISTS accepted_part_selectors text[] NOT NULL DEFAULT '{}'::text[];

ALTER TABLE corrections
  ADD COLUMN IF NOT EXISTS resulting_spec_id text;

ALTER TABLE corrections
  ADD COLUMN IF NOT EXISTS resulting_spec_version integer;

ALTER TABLE corrections
  ADD CONSTRAINT corrections_resulting_spec_pair_chk
    CHECK (
      (resulting_spec_id IS NULL AND resulting_spec_version IS NULL)
      OR (resulting_spec_id IS NOT NULL AND resulting_spec_version IS NOT NULL)
    );

ALTER TABLE corrections
  ADD CONSTRAINT corrections_resulting_spec_fkey
    FOREIGN KEY (tenant_id, resulting_spec_id, resulting_spec_version)
    REFERENCES reconstruction_specs(tenant_id, spec_id, version)
    ON DELETE SET NULL;

-- Index for moderator queue scan: open corrections by tenant, oldest
-- first.
CREATE INDEX IF NOT EXISTS corrections_status_open_idx
  ON corrections (tenant_id, status, created_at)
  WHERE status = 'open';

-- Index for rate-limit lookups: count submissions by IP hash in the
-- last hour. Partial index keeps it small (open + accepted only).
CREATE INDEX IF NOT EXISTS corrections_submitter_ip_hash_idx
  ON corrections (tenant_id, submitter_ip_hash, created_at)
  WHERE submitter_ip_hash IS NOT NULL;

-- 2. Rate-limit ledger for anonymous submissions.
--
-- Per Phase 4 spec: at most 10 anonymous submissions per IP per hour.
-- One row per (tenant, ip_hash, hour_bucket). Stamped via UPSERT on
-- each anonymous SubmitCorrection.

CREATE TABLE IF NOT EXISTS correction_rate_limits (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  submitter_ip_hash text NOT NULL,
  -- Hour bucket: floor(epoch_seconds / 3600). Stored as bigint for
  -- portability and so the rate-limit window can shift if needed.
  hour_bucket bigint NOT NULL,
  submission_count integer NOT NULL DEFAULT 1 CHECK (submission_count >= 0),
  created_at timestamptz NOT NULL DEFAULT now(),
  updated_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, submitter_ip_hash, hour_bucket)
);

CREATE INDEX IF NOT EXISTS correction_rate_limits_lookup_idx
  ON correction_rate_limits (tenant_id, submitter_ip_hash, hour_bucket);

ALTER TABLE correction_rate_limits ENABLE ROW LEVEL SECURITY;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'correction_rate_limits'
      AND policyname = 'correction_rate_limits_current'
  ) THEN
    CREATE POLICY correction_rate_limits_current ON correction_rate_limits
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

-- 3. Public changelog entries.
--
-- One row per approved correction that produced a publishable
-- change. Server-generated; never directly edited. RLS-scoped; the
-- public route reads this table with a read-only role that sets
-- app.tenant_id to the requested tenant.

CREATE TABLE IF NOT EXISTS changelog_entries (
  id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
  tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
  correction_id uuid NOT NULL,
  public_title text NOT NULL CHECK (public_title <> ''),
  public_summary text NOT NULL DEFAULT '',
  resulting_spec_id text,
  resulting_spec_version integer,
  published_at timestamptz NOT NULL DEFAULT now(),
  UNIQUE (tenant_id, id),
  -- One entry per correction, enforced at the row level. Re-publishing
  -- on retry is idempotent.
  UNIQUE (tenant_id, correction_id),
  FOREIGN KEY (tenant_id, correction_id) REFERENCES corrections(tenant_id, id) ON DELETE RESTRICT,
  CHECK (
    (resulting_spec_id IS NULL AND resulting_spec_version IS NULL)
    OR (resulting_spec_id IS NOT NULL AND resulting_spec_version IS NOT NULL)
  ),
  FOREIGN KEY (tenant_id, resulting_spec_id, resulting_spec_version)
    REFERENCES reconstruction_specs(tenant_id, spec_id, version) ON DELETE RESTRICT
);

CREATE INDEX IF NOT EXISTS changelog_entries_tenant_published_idx
  ON changelog_entries (tenant_id, published_at DESC);

CREATE INDEX IF NOT EXISTS changelog_entries_correction_idx
  ON changelog_entries (tenant_id, correction_id);

ALTER TABLE changelog_entries ENABLE ROW LEVEL SECURITY;

DO $$
BEGIN
  IF NOT EXISTS (
    SELECT 1 FROM pg_policies
    WHERE schemaname = current_schema()
      AND tablename = 'changelog_entries'
      AND policyname = 'changelog_entries_current'
  ) THEN
    CREATE POLICY changelog_entries_current ON changelog_entries
      USING (tenant_id::text = current_setting('app.tenant_id', true))
      WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
  END IF;
END
$$;

-- 4. Immutability trigger on accepted corrections.
--
-- Once a correction is accepted and produces a changelog entry, the
-- correction row's status, resulting_spec_id, and resulting_spec_version
-- must not change. Rejection and supersedence are still allowed:
-- moderators can supersede an accepted correction by accepting a
-- newer correction that targets the same entity. That happens
-- through a NEW correction row, not by mutating the old one.

CREATE OR REPLACE FUNCTION prevent_accepted_correction_mutation()
RETURNS trigger AS $$
BEGIN
  IF OLD.status = 'accepted' THEN
    IF NEW.status <> OLD.status
       OR NEW.resulting_spec_id IS DISTINCT FROM OLD.resulting_spec_id
       OR NEW.resulting_spec_version IS DISTINCT FROM OLD.resulting_spec_version
       OR NEW.accepted_part_selectors <> OLD.accepted_part_selectors
    THEN
      -- Status transition accepted -> superseded is permitted.
      IF NOT (NEW.status = 'superseded' AND OLD.status = 'accepted'
              AND NEW.resulting_spec_id = OLD.resulting_spec_id
              AND NEW.resulting_spec_version = OLD.resulting_spec_version) THEN
        RAISE EXCEPTION 'accepted corrections are immutable except for accepted -> superseded';
      END IF;
    END IF;
  END IF;
  RETURN NEW;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS corrections_accepted_immutable_update ON corrections;
CREATE TRIGGER corrections_accepted_immutable_update
  BEFORE UPDATE ON corrections
  FOR EACH ROW
  EXECUTE FUNCTION prevent_accepted_correction_mutation();
