-- Event email channel, outreach, and provider delivery events.
--
-- `event_application_backup_receipts` tracks whether the background worker
-- finished a receipt-delivery job. These tables track the actual email
-- channel and messages organizers see in the planner.

CREATE TABLE event_email_channels (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    event_layer_id uuid NOT NULL REFERENCES event_layers(id) ON DELETE CASCADE,
    provider text NOT NULL DEFAULT 'resend',
    sender_email text NOT NULL,
    sender_name text,
    reply_to_email text,
    reply_routing_mode text NOT NULL DEFAULT 'manual',
    delivery_webhook_status text NOT NULL DEFAULT 'not_configured',
    provider_deployment_label text,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1,
    UNIQUE (tenant_id, event_layer_id),
    CHECK (reply_routing_mode IN ('gmail_metadata', 'resend_inbound', 'manual'))
);

CREATE INDEX idx_event_email_channels_layer
    ON event_email_channels (tenant_id, event_layer_id);

CREATE TRIGGER event_email_channels_bump_version
    BEFORE UPDATE ON event_email_channels
    FOR EACH ROW EXECUTE FUNCTION bump_version();

ALTER TABLE event_email_channels ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation_event_email_channels
    ON event_email_channels
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

CREATE TABLE event_email_outreach (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    event_layer_id uuid NOT NULL REFERENCES event_layers(id) ON DELETE CASCADE,
    application_id uuid REFERENCES event_applications(id) ON DELETE SET NULL,
    recipient_email text NOT NULL,
    subject text NOT NULL,
    preview_text text,
    body_markdown text,
    resend_email_id text,
    message_id text,
    reply_to_email text,
    delivery_state text NOT NULL DEFAULT 'queued',
    reply_state text NOT NULL DEFAULT 'not_replied',
    notes_doc_id text,
    created_by_user_id uuid REFERENCES event_planner_users(id) ON DELETE SET NULL,
    idempotency_key text,
    sent_at timestamptz,
    last_event_at timestamptz,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    version bigint NOT NULL DEFAULT 1,
    CHECK (delivery_state IN (
        'queued',
        'sent',
        'delivered',
        'opened',
        'clicked',
        'delivery_delayed',
        'bounced',
        'complained',
        'failed',
        'suppressed',
        'received'
    )),
    CHECK (reply_state IN ('not_replied', 'replied', 'deferred', 'manual'))
);

CREATE INDEX idx_event_email_outreach_layer
    ON event_email_outreach (tenant_id, event_layer_id, created_at DESC);
CREATE INDEX idx_event_email_outreach_application
    ON event_email_outreach (tenant_id, application_id, created_at DESC)
    WHERE application_id IS NOT NULL;
CREATE UNIQUE INDEX idx_event_email_outreach_resend_email
    ON event_email_outreach (tenant_id, resend_email_id)
    WHERE resend_email_id IS NOT NULL;
CREATE UNIQUE INDEX idx_event_email_outreach_idempotency
    ON event_email_outreach (tenant_id, event_layer_id, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE TRIGGER event_email_outreach_bump_version
    BEFORE UPDATE ON event_email_outreach
    FOR EACH ROW EXECUTE FUNCTION bump_version();

ALTER TABLE event_email_outreach ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation_event_email_outreach
    ON event_email_outreach
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));

CREATE TABLE event_email_events (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    event_layer_id uuid REFERENCES event_layers(id) ON DELETE SET NULL,
    outreach_id uuid REFERENCES event_email_outreach(id) ON DELETE SET NULL,
    application_id uuid REFERENCES event_applications(id) ON DELETE SET NULL,
    provider text NOT NULL DEFAULT 'resend',
    provider_event_id text NOT NULL,
    resend_email_id text,
    event_type text NOT NULL,
    delivery_state text,
    payload_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    received_at timestamptz NOT NULL DEFAULT now(),
    event_at timestamptz,
    processed_at timestamptz,
    UNIQUE (tenant_id, provider, provider_event_id)
);

CREATE INDEX idx_event_email_events_outreach
    ON event_email_events (tenant_id, outreach_id, received_at DESC)
    WHERE outreach_id IS NOT NULL;
CREATE INDEX idx_event_email_events_resend_email
    ON event_email_events (tenant_id, resend_email_id, received_at DESC)
    WHERE resend_email_id IS NOT NULL;
CREATE INDEX idx_event_email_events_type
    ON event_email_events (tenant_id, event_type, received_at DESC);

ALTER TABLE event_email_events ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation_event_email_events
    ON event_email_events
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
