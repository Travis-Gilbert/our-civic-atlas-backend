-- Post-acceptance application billing.
--
-- Public application intake stays payment-free. Organizers request payment only
-- after review/acceptance, and this table records the Square payment-link
-- request plus the local idempotency key that made it safe to retry.

CREATE TABLE event_application_billing_requests (
    id uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    tenant_id uuid NOT NULL REFERENCES tenants(id) ON DELETE CASCADE,
    event_application_id uuid NOT NULL REFERENCES event_applications(id) ON DELETE CASCADE,
    provider text NOT NULL DEFAULT 'square',
    status text NOT NULL DEFAULT 'requested',
    amount_cents bigint NOT NULL CHECK (amount_cents > 0),
    currency text NOT NULL DEFAULT 'USD',
    payment_link_url text,
    provider_payment_link_id text,
    provider_order_id text,
    idempotency_key text NOT NULL,
    request_payload_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    response_payload_json jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_by uuid REFERENCES event_planner_users(id) ON DELETE SET NULL,
    created_at timestamptz NOT NULL DEFAULT now(),
    updated_at timestamptz NOT NULL DEFAULT now(),
    paid_at timestamptz,
    UNIQUE (tenant_id, event_application_id, provider, idempotency_key)
);

CREATE INDEX idx_event_application_billing_application
    ON event_application_billing_requests (tenant_id, event_application_id, created_at DESC);
CREATE INDEX idx_event_application_billing_status
    ON event_application_billing_requests (tenant_id, status, created_at DESC);
CREATE INDEX idx_event_application_billing_provider_link
    ON event_application_billing_requests (tenant_id, provider, provider_payment_link_id)
    WHERE provider_payment_link_id IS NOT NULL;

CREATE TRIGGER event_application_billing_bump_version
    BEFORE UPDATE ON event_application_billing_requests
    FOR EACH ROW EXECUTE FUNCTION bump_version();

ALTER TABLE event_application_billing_requests ENABLE ROW LEVEL SECURITY;

CREATE POLICY tenant_isolation_event_application_billing_requests
    ON event_application_billing_requests
    USING (tenant_id::text = current_setting('app.tenant_id', true))
    WITH CHECK (tenant_id::text = current_setting('app.tenant_id', true));
