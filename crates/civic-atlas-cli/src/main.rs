use clap::{Parser, Subcommand};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Parser)]
#[command(name = "civic-atlas")]
#[command(about = "Our Civic Atlas backend operations")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Tenant {
        #[command(subcommand)]
        command: TenantCommand,
    },
}

#[derive(Debug, Subcommand)]
enum TenantCommand {
    New {
        slug: String,
        #[arg(long)]
        display_name: Option<String>,
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Tenant {
            command:
                TenantCommand::New {
                    slug,
                    display_name,
                    database_url,
                },
        } => provision_tenant(&database_url, &slug, display_name.as_deref()).await?,
    }
    Ok(())
}

async fn provision_tenant(
    database_url: &str,
    slug: &str,
    display_name: Option<&str>,
) -> anyhow::Result<()> {
    validate_slug(slug)?;
    let pool = PgPool::connect(database_url).await?;
    let mut tx = pool.begin().await?;
    let tenant_id = Uuid::new_v4();
    let resolved_display_name = display_name.unwrap_or(slug);
    let namespace = format!("tenant:{slug}");

    sqlx::query(
        r#"
        INSERT INTO tenants (id, slug, display_name)
        VALUES ($1, $2, $3)
        "#,
    )
    .bind(tenant_id)
    .bind(slug)
    .bind(resolved_display_name)
    .execute(&mut *tx)
    .await?;

    sqlx::query(
        r#"
        INSERT INTO tenant_runtime_namespaces (tenant_id, rustyred_namespace)
        VALUES ($1, $2)
        "#,
    )
    .bind(tenant_id)
    .bind(&namespace)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    println!("{slug} {tenant_id} {namespace}");
    Ok(())
}

fn validate_slug(slug: &str) -> anyhow::Result<()> {
    let valid = !slug.is_empty()
        && slug
            .chars()
            .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
    anyhow::ensure!(
        valid,
        "tenant slug must be lowercase letters, digits, or hyphens"
    );
    Ok(())
}
