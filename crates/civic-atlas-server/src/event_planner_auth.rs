//! Phase 2 magic-link auth helpers.
//!
//! The Node sidecar holds the HTTP entry points (`POST /auth/claim`,
//! `POST /auth/sign-out`, cookie handling), but the actual token
//! hashing + invite/session table writes live here so the only
//! credentials in Node are session cookies the sidecar issues to its
//! own browser. Theseus credentials, Postgres credentials, and now
//! invite tokens all stay server-to-server.
//!
//! Token storage model:
//!   1. Inviter calls `invite_planner.py` which inserts a row with
//!      `token_hash = sha256(cleartext)` and prints the cleartext
//!      magic link to stdout.
//!   2. Inviter pastes that link to the planner (Slack/SMS/email).
//!   3. Planner clicks; their browser hits Next route
//!      `/open-flint-atlas/plan/auth/claim/[token]`.
//!   4. The Next route POSTs the cleartext token to the sidecar at
//!      `/auth/claim`.
//!   5. The sidecar hashes the token, looks it up via this module's
//!      `claim_invite` function, and on success creates a fresh
//!      session row + returns a session cookie token.
//!
//! Why hash before storing: a DB dump can't be replayed into the
//! product. The cleartext is only in memory while the script prints
//! it, then again briefly when a planner claims it.

#![allow(clippy::result_large_err)]

use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use tonic::Status;
use uuid::Uuid;

/// Hash a cleartext token with SHA-256 hex. Matches the format
/// `invite_planner.py` writes.
pub fn hash_token(token: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Result of consuming an invite token successfully: the user we
/// authenticated as, and the cleartext session token the caller
/// should set as an HTTP-only cookie on the browser.
pub struct ClaimedInvite {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub display_name: String,
    pub session_token: String,
}

/// Consume a magic-link invite token. Marks the invite as consumed,
/// upserts a planner row for the email, and creates a fresh session.
///
/// Returns `Ok(None)` when the token doesn't exist, is expired, or
/// was already consumed — the caller surfaces that as "magic link
/// expired" without leaking which case it was.
pub async fn claim_invite(
    pool: &PgPool,
    cleartext_token: &str,
) -> Result<Option<ClaimedInvite>, Status> {
    let token_hash = hash_token(cleartext_token);
    let mut tx = pool.begin().await.map_err(db_status)?;

    // Atomically mark the invite consumed (UPDATE only succeeds when
    // it wasn't consumed and isn't expired). Returning the invite row
    // gives us the email/display_name without a second SELECT.
    let invite = sqlx::query(
        r#"
        UPDATE event_planner_invites
        SET consumed_at = now()
        WHERE token_hash = $1
          AND consumed_at IS NULL
          AND expires_at > now()
        RETURNING tenant_id, email, display_name, invited_by
        "#,
    )
    .bind(&token_hash)
    .fetch_optional(&mut *tx)
    .await
    .map_err(db_status)?;

    let Some(invite_row) = invite else {
        tx.rollback().await.ok();
        return Ok(None);
    };

    let tenant_id: Uuid = invite_row.get("tenant_id");
    let email: String = invite_row.get("email");
    let display_name: String = invite_row.get("display_name");
    let invited_by: Option<Uuid> = invite_row.try_get("invited_by").ok();

    set_session_tenant(&mut tx, tenant_id).await?;

    // Upsert the planner user. Idempotent so a reissued invite to an
    // existing planner doesn't fail and doesn't change their id.
    let user_row = sqlx::query(
        r#"
        INSERT INTO event_planner_users (tenant_id, email, display_name, invited_by)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (tenant_id, email) DO UPDATE
          SET display_name = EXCLUDED.display_name
        RETURNING id, display_name
        "#,
    )
    .bind(tenant_id)
    .bind(&email)
    .bind(&display_name)
    .bind(invited_by)
    .fetch_one(&mut *tx)
    .await
    .map_err(db_status)?;
    let user_id: Uuid = user_row.get("id");
    let resolved_display_name: String = user_row.get("display_name");

    // Generate a 32-byte cleartext session token; the cookie carries
    // the cleartext, the DB carries only the hash.
    let session_token = random_token(32);
    let session_token_hash = hash_token(&session_token);

    sqlx::query(
        r#"
        INSERT INTO event_planner_sessions (token_hash, user_id, tenant_id, expires_at)
        VALUES ($1, $2, $3, now() + interval '30 days')
        "#,
    )
    .bind(&session_token_hash)
    .bind(user_id)
    .bind(tenant_id)
    .execute(&mut *tx)
    .await
    .map_err(db_status)?;

    tx.commit().await.map_err(db_status)?;

    Ok(Some(ClaimedInvite {
        user_id,
        tenant_id,
        display_name: resolved_display_name,
        session_token,
    }))
}

/// Look up the planner user behind a session cookie's cleartext
/// token. Returns Ok(None) if the session is missing or expired.
///
/// The sidecar calls this on every mutation request so it can hand
/// `actor_user_id` to the tonic mutations.
pub async fn resolve_session(
    pool: &PgPool,
    cleartext_token: &str,
) -> Result<Option<ResolvedSession>, Status> {
    if cleartext_token.is_empty() {
        return Ok(None);
    }
    let token_hash = hash_token(cleartext_token);

    let row = sqlx::query(
        r#"
        SELECT s.user_id, s.tenant_id, u.display_name, u.email
        FROM event_planner_sessions s
        JOIN event_planner_users u ON u.id = s.user_id
        WHERE s.token_hash = $1
          AND s.expires_at > now()
        "#,
    )
    .bind(&token_hash)
    .fetch_optional(pool)
    .await
    .map_err(db_status)?;

    let Some(row) = row else { return Ok(None) };

    Ok(Some(ResolvedSession {
        user_id: row.get("user_id"),
        tenant_id: row.get("tenant_id"),
        display_name: row.get("display_name"),
        email: row.get("email"),
    }))
}

/// Invalidate a session. Used by the sign-out route.
pub async fn revoke_session(pool: &PgPool, cleartext_token: &str) -> Result<(), Status> {
    if cleartext_token.is_empty() {
        return Ok(());
    }
    let token_hash = hash_token(cleartext_token);
    sqlx::query("DELETE FROM event_planner_sessions WHERE token_hash = $1")
        .bind(&token_hash)
        .execute(pool)
        .await
        .map_err(db_status)?;
    Ok(())
}

pub struct ResolvedSession {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub display_name: String,
    pub email: String,
}

/* ------------------------------------------------------------------ */
/*  Helpers                                                            */
/* ------------------------------------------------------------------ */

fn db_status(error: sqlx::Error) -> Status {
    Status::internal(format!("database error: {error}"))
}

async fn set_session_tenant(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: Uuid,
) -> Result<(), Status> {
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant_id.to_string())
        .execute(&mut **tx)
        .await
        .map_err(db_status)?;
    Ok(())
}

/// 32-byte token rendered as 64 hex chars. `getrandom`-backed via
/// the standard `rand` crate would be cleaner; using the chrono+pid
/// fallback would be insecure. We're already linking sha2; the
/// project workspace has `uuid` (which uses `getrandom`), so reuse it
/// twice for 32 bytes of entropy.
fn random_token(bytes: usize) -> String {
    let mut buf = Vec::with_capacity(bytes);
    while buf.len() < bytes {
        let chunk = Uuid::new_v4();
        buf.extend_from_slice(chunk.as_bytes());
    }
    buf.truncate(bytes);
    buf.iter().map(|b| format!("{b:02x}")).collect()
}
