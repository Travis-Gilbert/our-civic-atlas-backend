-- Civic Atlas event application intake.
--
-- Applications are captured before payment, email, notification, or map
-- placement work. The row is the durable intake ledger for the event-planning
-- platform; the companion backup receipt is the durable notification/outbox
-- record that later workers can forward to email, Sheets, or another operator
-- controlled store without blocking the submit path.

CREATE TABLE event_applications (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    event_layer_id uuid NOT NULL REFERENCES event_layers(id) ON DELETE CASCADE,
    category text NOT NULL,
    display_name text NOT NULL,
    contact_name text,
    contact_email text NOT NULL,
    contact_phone text,
    city text,
    bio text,
    flint_based boolean NOT NULL DEFAULT false,
    access_needs text,
    category_payload_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    planning_payload_json jsonb NOT NULL DEFAULT '{"accepted":false,"contacted":false,"paid":false,"fee":null,"payment_to_band":null,"location":null,"set_time":null,"status":"submitted"}'::jsonb,
    status text NOT NULL DEFAULT 'submitted',
    location geography(POINT, 4326),
    set_time timestamptz,
    source_key text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1,
    UNIQUE (tenant_id, event_layer_id, source_key)
);

CREATE INDEX idx_event_applications_layer
    ON event_applications (tenant_id, event_layer_id, created_at DESC);
CREATE INDEX idx_event_applications_email
    ON event_applications (tenant_id, lower(contact_email));
CREATE INDEX idx_event_applications_status
    ON event_applications (tenant_id, status);
CREATE INDEX idx_event_applications_category
    ON event_applications (tenant_id, category);
CREATE INDEX idx_event_applications_location
    ON event_applications USING gist (location)
    WHERE location IS NOT NULL;

CREATE TRIGGER event_applications_bump_version
    BEFORE UPDATE ON event_applications
    FOR EACH ROW EXECUTE FUNCTION bump_version();

ALTER TABLE event_applications ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation_event_applications ON event_applications
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

CREATE TABLE event_application_backup_receipts (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    event_application_id uuid NOT NULL REFERENCES event_applications(id) ON DELETE CASCADE,
    receipt_kind text NOT NULL DEFAULT 'operator_backup_notification',
    status text NOT NULL DEFAULT 'pending',
    payload_json jsonb NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    delivered_at timestamptz,
    UNIQUE (tenant_id, event_application_id, receipt_kind)
);

CREATE INDEX idx_event_application_backup_receipts_pending
    ON event_application_backup_receipts (tenant_id, status, created_at)
    WHERE status = 'pending';

ALTER TABLE event_application_backup_receipts ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation_event_application_backup_receipts
    ON event_application_backup_receipts
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
