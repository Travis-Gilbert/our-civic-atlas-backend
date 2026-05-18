use std::{fs, path::PathBuf};

use clap::{Parser, Subcommand};
use serde_json::Value;
use sqlx::{types::Json, PgPool, Postgres, Transaction};
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
    Spec {
        #[command(subcommand)]
        command: SpecCommand,
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

#[derive(Debug, Subcommand)]
enum SpecCommand {
    Validate {
        file: PathBuf,
    },
    Submit {
        file: PathBuf,
        #[arg(long, env = "DATABASE_URL")]
        database_url: String,
        #[arg(long, default_value = "cli")]
        submitted_by: String,
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
        Command::Spec { command } => match command {
            SpecCommand::Validate { file } => {
                let spec = load_spec_file(&file)?;
                let summary = validate_spec(&spec)?;
                println!(
                    "valid {} v{} tenant {}",
                    summary.spec_id, summary.version, summary.tenant_key
                );
            }
            SpecCommand::Submit {
                file,
                database_url,
                submitted_by,
            } => submit_spec(&database_url, &file, &submitted_by).await?,
        },
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

async fn submit_spec(database_url: &str, file: &PathBuf, submitted_by: &str) -> anyhow::Result<()> {
    let spec = load_spec_file(file)?;
    let summary = validate_spec(&spec)?;
    let pool = PgPool::connect(database_url).await?;
    let mut tx = pool.begin().await?;
    let tenant_id = resolve_tenant_id(&mut tx, &summary.tenant_key).await?;

    sqlx::query("SELECT set_config('app.tenant_id', $1, true)")
        .bind(tenant_id.to_string())
        .execute(&mut *tx)
        .await?;

    let building_id = optional_uuid_field(&spec, &["buildingId", "building_id"])?;
    let parcel_id = optional_uuid_field(&spec, &["parcelId", "parcel_id"])?;
    let block_id = optional_string_field(&spec, &["blockId", "block_id"]);
    let supersedes_spec_id =
        optional_string_field(&spec, &["supersedesSpecId", "supersedes_spec_id"]);

    sqlx::query(
        r#"
        INSERT INTO reconstruction_specs (
          tenant_id,
          spec_id,
          version,
          status,
          building_id,
          parcel_id,
          civic_object_id,
          block_id,
          title,
          supersedes_spec_id,
          spec_jsonb,
          created_by,
          updated_at
        )
        VALUES ($1, $2, $3, 'in_review', $4, $5, $6, $7, $8, $9, $10, $11, now())
        ON CONFLICT (tenant_id, spec_id, version) DO UPDATE
        SET status = 'in_review',
            building_id = EXCLUDED.building_id,
            parcel_id = EXCLUDED.parcel_id,
            civic_object_id = EXCLUDED.civic_object_id,
            block_id = EXCLUDED.block_id,
            title = EXCLUDED.title,
            supersedes_spec_id = EXCLUDED.supersedes_spec_id,
            spec_jsonb = EXCLUDED.spec_jsonb,
            created_by = EXCLUDED.created_by,
            updated_at = now()
        WHERE reconstruction_specs.status <> 'approved'
        "#,
    )
    .bind(tenant_id)
    .bind(&summary.spec_id)
    .bind(summary.version)
    .bind(building_id)
    .bind(parcel_id)
    .bind(&summary.civic_object_id)
    .bind(block_id)
    .bind(&summary.title)
    .bind(supersedes_spec_id)
    .bind(Json(&spec))
    .bind(submitted_by)
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    println!(
        "submitted {} v{} tenant {}",
        summary.spec_id, summary.version, summary.tenant_key
    );
    Ok(())
}

async fn resolve_tenant_id(
    tx: &mut Transaction<'_, Postgres>,
    tenant_key: &str,
) -> anyhow::Result<Uuid> {
    let tenant_id: Option<Uuid> = sqlx::query_scalar(
        r#"
        SELECT id
        FROM tenants
        WHERE slug = $1 OR id::text = $1
        "#,
    )
    .bind(tenant_key)
    .fetch_optional(&mut **tx)
    .await?;
    tenant_id.ok_or_else(|| anyhow::anyhow!("tenant not found: {tenant_key}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SpecSummary {
    tenant_key: String,
    spec_id: String,
    civic_object_id: String,
    title: String,
    version: i32,
}

fn load_spec_file(file: &PathBuf) -> anyhow::Result<Value> {
    let raw = fs::read_to_string(file)?;
    Ok(serde_json::from_str(&raw)?)
}

fn validate_spec(spec: &Value) -> anyhow::Result<SpecSummary> {
    let tenant_context = object_field(spec, &["tenantContext", "tenant_context"])?;
    let tenant_key = required_string_field(tenant_context, &["tenantId", "tenant_id"])?;
    validate_slug_like(&tenant_key, "tenant_id")?;

    let spec_id = required_string_field(spec, &["specId", "spec_id"])?;
    let civic_object_id = required_string_field(spec, &["civicObjectId", "civic_object_id"])?;
    let title = required_string_field(spec, &["title"])?;
    let version = integer_field(spec, &["version"])?.unwrap_or(1);
    anyhow::ensure!(version > 0, "version must be greater than zero");

    validate_optional_confidence(spec, &["mass", "provenance", "confidence"])?;
    validate_optional_confidence(spec, &["roof", "provenance", "confidence"])?;
    validate_optional_confidence(spec, &["groundFloor", "provenance", "confidence"])?;
    validate_optional_confidence(spec, &["ground_floor", "provenance", "confidence"])?;
    validate_part_array_confidence(spec, &["facades"])?;
    validate_part_array_confidence(spec, &["ornaments"])?;
    validate_opening_grid_confidence(spec)?;

    Ok(SpecSummary {
        tenant_key,
        spec_id,
        civic_object_id,
        title,
        version,
    })
}

fn object_field<'a>(value: &'a Value, names: &[&str]) -> anyhow::Result<&'a Value> {
    names
        .iter()
        .find_map(|name| value.get(*name))
        .filter(|field| field.is_object())
        .ok_or_else(|| anyhow::anyhow!("missing object field: {}", names.join(" or ")))
}

fn required_string_field(value: &Value, names: &[&str]) -> anyhow::Result<String> {
    let field = optional_string_field(value, names)
        .ok_or_else(|| anyhow::anyhow!("missing string field: {}", names.join(" or ")))?;
    anyhow::ensure!(!field.trim().is_empty(), "{} cannot be empty", names[0]);
    Ok(field)
}

fn optional_string_field(value: &Value, names: &[&str]) -> Option<String> {
    names
        .iter()
        .find_map(|name| value.get(*name))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
}

fn integer_field(value: &Value, names: &[&str]) -> anyhow::Result<Option<i32>> {
    let Some(field) = names.iter().find_map(|name| value.get(*name)) else {
        return Ok(None);
    };
    let number = field
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("{} must be an integer", names[0]))?;
    let number = i32::try_from(number)?;
    Ok(Some(number))
}

fn optional_uuid_field(value: &Value, names: &[&str]) -> anyhow::Result<Option<Uuid>> {
    let Some(raw) = optional_string_field(value, names) else {
        return Ok(None);
    };
    Ok(Some(raw.parse()?))
}

fn validate_optional_confidence(spec: &Value, path: &[&str]) -> anyhow::Result<()> {
    let Some(value) = path.iter().try_fold(spec, |current, key| current.get(*key)) else {
        return Ok(());
    };
    let confidence = value
        .as_f64()
        .ok_or_else(|| anyhow::anyhow!("{} must be a number", path.join(".")))?;
    anyhow::ensure!(
        (0.0..=1.0).contains(&confidence),
        "{} must be between 0 and 1",
        path.join(".")
    );
    Ok(())
}

fn validate_part_array_confidence(spec: &Value, names: &[&str]) -> anyhow::Result<()> {
    let Some(parts) = names.iter().find_map(|name| spec.get(*name)) else {
        return Ok(());
    };
    let parts = parts
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("{} must be an array", names[0]))?;
    for (index, part) in parts.iter().enumerate() {
        validate_optional_confidence(part, &["provenance", "confidence"])
            .map_err(|err| anyhow::anyhow!("{}[{index}]: {err}", names[0]))?;
    }
    Ok(())
}

fn validate_opening_grid_confidence(spec: &Value) -> anyhow::Result<()> {
    let Some(facades) = spec.get("facades").and_then(Value::as_array) else {
        return Ok(());
    };
    for (facade_index, facade) in facades.iter().enumerate() {
        let Some(grids) = facade
            .get("openingGrids")
            .or_else(|| facade.get("opening_grids"))
            .and_then(Value::as_array)
        else {
            continue;
        };
        for (grid_index, grid) in grids.iter().enumerate() {
            validate_optional_confidence(grid, &["provenance", "confidence"]).map_err(|err| {
                anyhow::anyhow!("facades[{facade_index}].openingGrids[{grid_index}]: {err}")
            })?;
        }
    }
    Ok(())
}

fn validate_slug_like(value: &str, label: &str) -> anyhow::Result<()> {
    let valid = !value.is_empty()
        && value.chars().all(|ch| {
            ch.is_ascii_lowercase()
                || ch.is_ascii_digit()
                || ch == '-'
                || (label == "tenant_id" && (ch == '_' || ch.is_ascii_hexdigit()))
        });
    anyhow::ensure!(valid, "{label} contains unsupported characters");
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validate_spec_accepts_part_level_confidence() {
        let spec = json!({
            "tenantContext": {"tenantId": "flint"},
            "specId": "carriage-town-001",
            "civicObjectId": "building:001",
            "title": "Carriage Town storefront",
            "version": 2,
            "mass": {
                "provenance": {"confidence": 0.82}
            },
            "facades": [{
                "provenance": {"confidence": 0.7},
                "openingGrids": [{
                    "provenance": {"confidence": 0.6}
                }]
            }],
            "roof": {
                "provenance": {"confidence": 0.5}
            }
        });

        let summary = validate_spec(&spec).expect("spec validates");

        assert_eq!(
            summary,
            SpecSummary {
                tenant_key: "flint".to_string(),
                spec_id: "carriage-town-001".to_string(),
                civic_object_id: "building:001".to_string(),
                title: "Carriage Town storefront".to_string(),
                version: 2,
            }
        );
    }

    #[test]
    fn validate_spec_rejects_invalid_confidence() {
        let spec = json!({
            "tenant_context": {"tenant_id": "flint"},
            "spec_id": "carriage-town-001",
            "civic_object_id": "building:001",
            "title": "Carriage Town storefront",
            "version": 1,
            "ground_floor": {
                "provenance": {"confidence": 1.2}
            }
        });

        let error = validate_spec(&spec).expect_err("invalid confidence fails");

        assert!(error
            .to_string()
            .contains("ground_floor.provenance.confidence must be between 0 and 1"));
    }
}
