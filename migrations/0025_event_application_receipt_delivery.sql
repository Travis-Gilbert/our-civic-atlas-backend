-- Retry metadata for Porchfest application receipt delivery.
--
-- The submit path writes event_application_backup_receipts inside the same
-- transaction as event_applications. This migration lets the outbox worker
-- deliver those receipts as emails after capture without blocking applicant
-- submission on an email provider.

ALTER TABLE event_application_backup_receipts
    ADD COLUMN attempt_count integer NOT NULL DEFAULT 0,
    ADD COLUMN last_error text,
    ADD COLUMN next_attempt_at timestamptz;

CREATE INDEX idx_event_application_backup_receipts_retry
    ON event_application_backup_receipts (tenant_id, status, next_attempt_at, created_at)
    WHERE status IN ('pending', 'running');
