-- Phase 2 magic-link auth, scoped tight. Five tables — wait, three
-- tables, plus an index on session expiry for the periodic
-- garbage-collection scan.
--
-- Design notes:
--
-- * Tokens are stored as SHA-256 hashes (TEXT), never as cleartext.
--   The script that prints magic links holds the cleartext token in
--   memory only long enough to print it. The DB never sees it
--   plaintext, so a DB dump doesn't leak active sessions.
--
-- * One user per (tenant, email). The same email can be a planner
--   on multiple tenants without collision, but on a single tenant
--   it's idempotent.
--
-- * `event_planner_sessions.expires_at` indexed so the periodic
--   sweeper can prune expired sessions efficiently.
--
-- * Spec said `tenant_id TEXT REFERENCES tenants(id)`; the existing
--   schema uses `tenant_id uuid REFERENCES tenants(id)` because
--   `tenants.id` is uuid. Using uuid here for FK type consistency.
--
-- * Spec called for migration 0013; bumped to 0014 since 0011/0012/
--   0013 are now taken by event_layers, versions, and notify
--   respectively.

CREATE TABLE event_planner_users (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    email text NOT NULL,
    display_name text NOT NULL,
    invited_by uuid REFERENCES event_planner_users(id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, email)
);

CREATE TABLE event_planner_invites (
    token_hash text PRIMARY KEY,
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    email text NOT NULL,
    display_name text NOT NULL,
    invited_by uuid REFERENCES event_planner_users(id) ON DELETE SET NULL,
    expires_at timestamptz NOT NULL,
    consumed_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_event_planner_invites_email
    ON event_planner_invites (tenant_id, email)
    WHERE consumed_at IS NULL;

CREATE TABLE event_planner_sessions (
    token_hash text PRIMARY KEY,
    user_id uuid NOT NULL REFERENCES event_planner_users(id) ON DELETE CASCADE,
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    expires_at timestamptz NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);

CREATE INDEX idx_event_planner_sessions_user
    ON event_planner_sessions (user_id);

CREATE INDEX idx_event_planner_sessions_expiry
    ON event_planner_sessions (expires_at);

ALTER TABLE event_planner_users ENABLE ROW LEVEL SECURITY;
ALTER TABLE event_planner_invites ENABLE ROW LEVEL SECURITY;
ALTER TABLE event_planner_sessions ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation_event_planner_users ON event_planner_users
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

CREATE POLICY tenant_isolation_event_planner_invites ON event_planner_invites
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

CREATE POLICY tenant_isolation_event_planner_sessions ON event_planner_sessions
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

-- Now that planners exist, hook the existing owner_user_id columns
-- up to their target table via a deferred FK. Deferred so the
-- column population can happen lazily — old rows may still have
-- NULL until the planner who created them re-saves.
ALTER TABLE event_placements
    ADD CONSTRAINT fk_event_placements_owner
    FOREIGN KEY (owner_user_id)
    REFERENCES event_planner_users(id)
    ON DELETE SET NULL
    DEFERRABLE INITIALLY DEFERRED;

ALTER TABLE event_tasks
    ADD CONSTRAINT fk_event_tasks_owner
    FOREIGN KEY (owner_user_id)
    REFERENCES event_planner_users(id)
    ON DELETE SET NULL
    DEFERRABLE INITIALLY DEFERRED;
