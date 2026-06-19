//! Optional Valkey helpers for disposable production acceleration.
//!
//! Postgres remains the source of truth for applications and outbox rows.
//! These helpers only provide short-TTL read-through cache entries and
//! best-effort rate counters; Valkey failures intentionally degrade open.

use std::env;

use civic_atlas_types::event_planner::EventApplicationListResponse;
use prost::Message;
use sha2::{Digest, Sha256};
use tonic::Status;
use tracing::{info, warn};
use uuid::Uuid;

const DEFAULT_KEY_PREFIX: &str = "civic-atlas";
const DEFAULT_APPLICATION_LIST_TTL_SECS: u64 = 10;
const DEFAULT_SUBMIT_RATE_LIMIT: i64 = 8;
const DEFAULT_SUBMIT_RATE_WINDOW_SECS: u64 = 600;
const MIN_VERSION_TTL_SECS: u64 = 3_600;

#[derive(Clone)]
pub struct ValkeyClient {
    client: redis::Client,
    key_prefix: String,
    application_list_ttl_secs: u64,
    submit_rate_limit: i64,
    submit_rate_window_secs: u64,
}

impl ValkeyClient {
    pub fn from_env() -> Option<Self> {
        let url = match env::var("VALKEY_URL") {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return None,
        };

        let client = match redis::Client::open(url.trim()) {
            Ok(client) => client,
            Err(error) => {
                warn!(%error, "VALKEY_URL is invalid; Valkey helpers disabled");
                return None;
            }
        };

        let key_prefix = env::var("VALKEY_KEY_PREFIX")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_KEY_PREFIX.to_string());
        let application_list_ttl_secs = env_u64(
            "VALKEY_EVENT_APPLICATION_LIST_TTL_SECS",
            DEFAULT_APPLICATION_LIST_TTL_SECS,
        );
        let submit_rate_limit = env_i64(
            "VALKEY_EVENT_APPLICATION_SUBMIT_RATE_LIMIT",
            DEFAULT_SUBMIT_RATE_LIMIT,
        );
        let submit_rate_window_secs = env_u64(
            "VALKEY_EVENT_APPLICATION_SUBMIT_RATE_WINDOW_SECS",
            DEFAULT_SUBMIT_RATE_WINDOW_SECS,
        );

        info!(
            application_list_ttl_secs,
            submit_rate_limit, submit_rate_window_secs, "Valkey helpers enabled"
        );

        Some(Self {
            client,
            key_prefix: clean_key_component(&key_prefix),
            application_list_ttl_secs,
            submit_rate_limit,
            submit_rate_window_secs,
        })
    }

    pub async fn get_event_applications(
        &self,
        tenant_id: Uuid,
        event_layer_id: Uuid,
        category: &str,
        status: &str,
    ) -> Option<EventApplicationListResponse> {
        if self.application_list_ttl_secs == 0 {
            return None;
        }

        let mut connection = match self.connection().await {
            Ok(connection) => connection,
            Err(error) => {
                warn!(%error, "Valkey unavailable; skipping application list cache read");
                return None;
            }
        };
        let version = match self
            .event_applications_version(&mut connection, tenant_id, event_layer_id)
            .await
        {
            Ok(version) => version,
            Err(error) => {
                warn!(%error, "Valkey application list version read failed");
                return None;
            }
        };
        let key =
            self.event_applications_key(tenant_id, event_layer_id, category, status, &version);
        let bytes: Option<Vec<u8>> = match redis::cmd("GET")
            .arg(&key)
            .query_async(&mut connection)
            .await
        {
            Ok(bytes) => bytes,
            Err(error) => {
                warn!(%error, "Valkey application list cache read failed");
                return None;
            }
        };

        bytes.and_then(
            |bytes| match EventApplicationListResponse::decode(bytes.as_slice()) {
                Ok(response) => Some(response),
                Err(error) => {
                    warn!(%error, "Valkey application list cache payload was invalid");
                    None
                }
            },
        )
    }

    pub async fn put_event_applications(
        &self,
        tenant_id: Uuid,
        event_layer_id: Uuid,
        category: &str,
        status: &str,
        response: &EventApplicationListResponse,
    ) {
        if self.application_list_ttl_secs == 0 {
            return;
        }

        let mut connection = match self.connection().await {
            Ok(connection) => connection,
            Err(error) => {
                warn!(%error, "Valkey unavailable; skipping application list cache write");
                return;
            }
        };
        let version = match self
            .event_applications_version(&mut connection, tenant_id, event_layer_id)
            .await
        {
            Ok(version) => version,
            Err(error) => {
                warn!(%error, "Valkey application list version read failed");
                return;
            }
        };
        let key =
            self.event_applications_key(tenant_id, event_layer_id, category, status, &version);
        let payload = response.encode_to_vec();
        if let Err(error) = redis::cmd("SET")
            .arg(&key)
            .arg(payload)
            .arg("EX")
            .arg(self.application_list_ttl_secs)
            .query_async::<()>(&mut connection)
            .await
        {
            warn!(%error, "Valkey application list cache write failed");
        }
    }

    pub async fn invalidate_event_applications(&self, tenant_id: Uuid, event_layer_id: Uuid) {
        let mut connection = match self.connection().await {
            Ok(connection) => connection,
            Err(error) => {
                warn!(%error, "Valkey unavailable; skipping application list cache invalidation");
                return;
            }
        };
        let key = self.event_applications_version_key(tenant_id, event_layer_id);
        let version_ttl = self
            .application_list_ttl_secs
            .saturating_mul(6)
            .max(MIN_VERSION_TTL_SECS);
        let result: redis::RedisResult<(i64, bool)> = redis::pipe()
            .atomic()
            .cmd("INCR")
            .arg(&key)
            .cmd("EXPIRE")
            .arg(&key)
            .arg(version_ttl)
            .query_async(&mut connection)
            .await;
        if let Err(error) = result {
            warn!(%error, "Valkey application list cache invalidation failed");
        }
    }

    pub async fn check_public_application_submit_rate(
        &self,
        tenant_slug: &str,
        event_layer_slug: &str,
        contact_email: &str,
    ) -> Result<(), Status> {
        if self.submit_rate_limit <= 0 || self.submit_rate_window_secs == 0 {
            return Ok(());
        }

        let mut connection = match self.connection().await {
            Ok(connection) => connection,
            Err(error) => {
                warn!(%error, "Valkey unavailable; allowing application submit");
                return Ok(());
            }
        };
        let key = format!(
            "{}:event-application-submit-rate:{}:{}:{}",
            self.key_prefix,
            clean_key_component(tenant_slug),
            clean_key_component(event_layer_slug),
            hash_component(contact_email),
        );
        let result: redis::RedisResult<(i64, bool)> = redis::pipe()
            .atomic()
            .cmd("INCR")
            .arg(&key)
            .cmd("EXPIRE")
            .arg(&key)
            .arg(self.submit_rate_window_secs)
            .query_async(&mut connection)
            .await;

        let (count, _) = match result {
            Ok(result) => result,
            Err(error) => {
                warn!(%error, "Valkey submit rate check failed; allowing application submit");
                return Ok(());
            }
        };
        if count > self.submit_rate_limit {
            return Err(Status::resource_exhausted(
                "too many application submissions for this email; please wait a few minutes and try again",
            ));
        }
        Ok(())
    }

    async fn connection(&self) -> redis::RedisResult<redis::aio::MultiplexedConnection> {
        self.client.get_multiplexed_async_connection().await
    }

    async fn event_applications_version(
        &self,
        connection: &mut redis::aio::MultiplexedConnection,
        tenant_id: Uuid,
        event_layer_id: Uuid,
    ) -> redis::RedisResult<String> {
        let key = self.event_applications_version_key(tenant_id, event_layer_id);
        redis::cmd("GET")
            .arg(&key)
            .query_async::<Option<String>>(connection)
            .await
            .map(|value| value.unwrap_or_else(|| "0".to_string()))
    }

    fn event_applications_version_key(&self, tenant_id: Uuid, event_layer_id: Uuid) -> String {
        format!(
            "{}:event-applications-version:{}:{}",
            self.key_prefix, tenant_id, event_layer_id
        )
    }

    fn event_applications_key(
        &self,
        tenant_id: Uuid,
        event_layer_id: Uuid,
        category: &str,
        status: &str,
        version: &str,
    ) -> String {
        format!(
            "{}:event-applications:{}:{}:{}:{}:{}",
            self.key_prefix,
            tenant_id,
            event_layer_id,
            clean_key_component(category),
            clean_key_component(status),
            clean_key_component(version),
        )
    }
}

fn env_u64(name: &str, default: u64) -> u64 {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .unwrap_or(default)
}

fn env_i64(name: &str, default: i64) -> i64 {
    env::var(name)
        .ok()
        .and_then(|value| value.trim().parse::<i64>().ok())
        .unwrap_or(default)
}

fn clean_key_component(value: &str) -> String {
    let cleaned = value
        .trim()
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if cleaned.is_empty() {
        "_".to_string()
    } else {
        cleaned
    }
}

fn hash_component(value: &str) -> String {
    let digest = Sha256::digest(value.trim().to_lowercase().as_bytes());
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_keys_are_stable_and_sanitized() {
        let client = ValkeyClient {
            client: redis::Client::open("redis://127.0.0.1/").unwrap(),
            key_prefix: "civic-atlas".to_string(),
            application_list_ttl_secs: 10,
            submit_rate_limit: 8,
            submit_rate_window_secs: 600,
        };
        let tenant_id = Uuid::parse_str("00000000-0000-4000-8000-000000000001").unwrap();
        let layer_id = Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap();

        assert_eq!(
            client.event_applications_key(tenant_id, layer_id, " vendor/food ", " submitted ", "0"),
            "civic-atlas:event-applications:00000000-0000-4000-8000-000000000001:00000000-0000-4000-8000-000000000002:vendor_food:submitted:0"
        );
    }

    #[test]
    fn submit_rate_key_hash_does_not_expose_email() {
        let hashed = hash_component("Applicant@Example.COM ");

        assert_eq!(hashed.len(), 64);
        assert!(!hashed.contains("Applicant"));
        assert_eq!(hashed, hash_component("applicant@example.com"));
    }
}
