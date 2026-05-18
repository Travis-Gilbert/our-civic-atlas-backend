use sqlx::{Postgres, Transaction};

pub async fn set_transaction_tenant(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant_id)
        .execute(&mut **tx)
        .await?;
    Ok(())
}
