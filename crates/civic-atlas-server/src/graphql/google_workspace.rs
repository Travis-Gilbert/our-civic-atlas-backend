//! Google Workspace/Sheets sync surface for event planning.
//!
//! This module keeps Google credentials on the Axum backend. The frontend only
//! sees GraphQL status, imported civic row fields, and explicit export results.

use std::{collections::BTreeSet, env};

use async_graphql::{Context, InputObject, Json, Object, SimpleObject};
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};

const GOOGLE_TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const GOOGLE_SHEETS_BASE: &str = "https://sheets.googleapis.com/v4/spreadsheets";

const DEFAULT_EVENT_SLUG: &str = "porchfest-2026";
const DEFAULT_TENANT_SLUG: &str = "flint";
const DEFAULT_TARGET_KIND: &str = "applications";
const DEFAULT_RANGE: &str = "Applications!A:Z";

const PREFERRED_EXPORT_HEADERS: &[&str] = &[
    "sourceId",
    "category",
    "name",
    "email",
    "phone",
    "city",
    "bio",
    "flintBased",
    "accessNeeds",
    "submittedAt",
    "artistName",
    "genre",
    "businessName",
    "foodDescription",
    "vendorNeeds",
    "orgName",
    "tier",
    "tierPrice",
    "sponsoringAs",
];

#[derive(SimpleObject, Clone)]
pub struct GoogleWorkspaceConnection {
    pub tenant_slug: String,
    pub event_slug: String,
    pub status: String,
    pub google_account_email: Option<String>,
    pub scopes: Vec<String>,
    pub sheets_configured: bool,
    pub gmail_configured: bool,
    pub linked_spreadsheet_id: Option<String>,
    pub default_sheet_range: Option<String>,
    pub last_checked_at: Option<DateTime<Utc>>,
    pub message: Option<String>,
}

#[derive(SimpleObject, Clone)]
pub struct GoogleSheetCivicRow {
    pub source_id: String,
    pub row_number: i32,
    pub target_kind: String,
    pub fields: Json<Value>,
    pub row_hash: String,
}

#[derive(SimpleObject, Clone)]
pub struct GoogleSheetSyncResult {
    pub connection: GoogleWorkspaceConnection,
    pub rows: Vec<GoogleSheetCivicRow>,
    pub row_count: i32,
    pub imported_at: Option<DateTime<Utc>>,
    pub message: Option<String>,
}

#[derive(SimpleObject, Clone)]
pub struct GoogleSheetExportResult {
    pub connection: GoogleWorkspaceConnection,
    pub dry_run: bool,
    pub row_count: i32,
    pub updated_range: Option<String>,
    pub updated_cells: i32,
    pub exported_at: Option<DateTime<Utc>>,
    pub message: Option<String>,
}

#[derive(InputObject)]
pub struct GoogleSheetSyncInput {
    pub tenant_slug: Option<String>,
    pub event_slug: Option<String>,
    pub spreadsheet_id: Option<String>,
    pub range: Option<String>,
    pub target_kind: Option<String>,
}

#[derive(InputObject)]
pub struct GoogleSheetCivicRowInput {
    pub source_id: Option<String>,
    pub fields: Json<Value>,
}

#[derive(InputObject)]
pub struct ExportCivicRowsToGoogleSheetInput {
    pub tenant_slug: Option<String>,
    pub event_slug: Option<String>,
    pub spreadsheet_id: Option<String>,
    pub range: Option<String>,
    pub target_kind: Option<String>,
    pub rows: Vec<GoogleSheetCivicRowInput>,
    pub dry_run: Option<bool>,
}

#[derive(Default)]
pub struct GoogleWorkspaceQuery;

#[Object]
impl GoogleWorkspaceQuery {
    async fn google_workspace_connection(
        &self,
        _ctx: &Context<'_>,
        #[graphql(default = "flint")] tenant_slug: String,
        event_slug: String,
    ) -> async_graphql::Result<GoogleWorkspaceConnection> {
        Ok(connection_from_env(
            clean_or_default(Some(tenant_slug), DEFAULT_TENANT_SLUG),
            clean_or_default(Some(event_slug), DEFAULT_EVENT_SLUG),
        ))
    }
}

#[derive(Default)]
pub struct GoogleWorkspaceMutation;

#[Object]
impl GoogleWorkspaceMutation {
    async fn sync_google_sheet_civic_rows(
        &self,
        _ctx: &Context<'_>,
        input: GoogleSheetSyncInput,
    ) -> async_graphql::Result<GoogleSheetSyncResult> {
        let tenant_slug = clean_or_default(input.tenant_slug, DEFAULT_TENANT_SLUG);
        let event_slug = clean_or_default(input.event_slug, DEFAULT_EVENT_SLUG);
        let target_kind = clean_or_default(input.target_kind, DEFAULT_TARGET_KIND);
        let spreadsheet_id = input
            .spreadsheet_id
            .filter(|value| !value.trim().is_empty())
            .or_else(|| env::var("PORCHFEST_GOOGLE_SHEET_ID").ok())
            .unwrap_or_default();
        let range = input
            .range
            .filter(|value| !value.trim().is_empty())
            .or_else(|| env::var("PORCHFEST_GOOGLE_SHEET_RANGE").ok())
            .unwrap_or_else(|| DEFAULT_RANGE.to_string());
        let connection = connection_from_env(tenant_slug, event_slug);

        if spreadsheet_id.trim().is_empty() || !connection.sheets_configured {
            return Ok(GoogleSheetSyncResult {
                connection,
                rows: Vec::new(),
                row_count: 0,
                imported_at: None,
                message: Some("Google Sheets sync is not configured on the backend".to_string()),
            });
        }

        let token = google_access_token().await?;
        let rows = fetch_sheet_rows(&token, &spreadsheet_id, &range, &target_kind).await?;
        let row_count = rows.len() as i32;
        Ok(GoogleSheetSyncResult {
            connection,
            rows,
            row_count,
            imported_at: Some(Utc::now()),
            message: Some(format!("Imported {row_count} Google Sheet row(s)")),
        })
    }

    async fn export_civic_rows_to_google_sheet(
        &self,
        _ctx: &Context<'_>,
        input: ExportCivicRowsToGoogleSheetInput,
    ) -> async_graphql::Result<GoogleSheetExportResult> {
        let tenant_slug = clean_or_default(input.tenant_slug, DEFAULT_TENANT_SLUG);
        let event_slug = clean_or_default(input.event_slug, DEFAULT_EVENT_SLUG);
        let spreadsheet_id = input
            .spreadsheet_id
            .filter(|value| !value.trim().is_empty())
            .or_else(|| env::var("PORCHFEST_GOOGLE_SHEET_ID").ok())
            .unwrap_or_default();
        let range = input
            .range
            .filter(|value| !value.trim().is_empty())
            .or_else(|| env::var("PORCHFEST_GOOGLE_SHEET_EXPORT_RANGE").ok())
            .or_else(|| env::var("PORCHFEST_GOOGLE_SHEET_RANGE").ok())
            .unwrap_or_else(|| DEFAULT_RANGE.to_string());
        let dry_run = input.dry_run.unwrap_or(true);
        let connection = connection_from_env(tenant_slug, event_slug);
        let values = export_values(&input.rows);
        let row_count = values.len().saturating_sub(1) as i32;

        if dry_run {
            return Ok(GoogleSheetExportResult {
                connection,
                dry_run,
                row_count,
                updated_range: Some(range),
                updated_cells: 0,
                exported_at: None,
                message: Some(format!("Prepared {row_count} row(s) for explicit export")),
            });
        }

        if spreadsheet_id.trim().is_empty() || !connection.sheets_configured {
            return Ok(GoogleSheetExportResult {
                connection,
                dry_run,
                row_count,
                updated_range: None,
                updated_cells: 0,
                exported_at: None,
                message: Some("Google Sheets export is not configured on the backend".to_string()),
            });
        }

        let token = google_access_token().await?;
        let updated = update_sheet_values(&token, &spreadsheet_id, &range, values).await?;
        Ok(GoogleSheetExportResult {
            connection,
            dry_run,
            row_count,
            updated_range: updated.updated_range,
            updated_cells: updated.updated_cells.unwrap_or_default(),
            exported_at: Some(Utc::now()),
            message: Some(format!("Updated {row_count} Google Sheet row(s)")),
        })
    }
}

fn connection_from_env(tenant_slug: String, event_slug: String) -> GoogleWorkspaceConnection {
    let has_client_id = env_present("GOOGLE_WORKSPACE_CLIENT_ID");
    let has_client_secret = env_present("GOOGLE_WORKSPACE_CLIENT_SECRET");
    let has_refresh_token = env_present("GOOGLE_WORKSPACE_REFRESH_TOKEN");
    let has_sheet = env_present("PORCHFEST_GOOGLE_SHEET_ID");
    let sheets_configured = has_client_id && has_client_secret && has_refresh_token && has_sheet;
    let gmail_configured = sheets_configured && env_present("PORCHFEST_GMAIL_HISTORY_ID");
    let mut scopes = vec![
        "https://www.googleapis.com/auth/spreadsheets".to_string(),
        "https://www.googleapis.com/auth/drive.file".to_string(),
    ];
    if gmail_configured {
        scopes.push("https://www.googleapis.com/auth/gmail.metadata".to_string());
    }
    GoogleWorkspaceConnection {
        tenant_slug,
        event_slug,
        status: if sheets_configured {
            "CONNECTED".to_string()
        } else {
            "NOT_CONFIGURED".to_string()
        },
        google_account_email: env::var("GOOGLE_WORKSPACE_ACCOUNT_EMAIL").ok(),
        scopes,
        sheets_configured,
        gmail_configured,
        linked_spreadsheet_id: env::var("PORCHFEST_GOOGLE_SHEET_ID").ok(),
        default_sheet_range: env::var("PORCHFEST_GOOGLE_SHEET_RANGE")
            .ok()
            .or_else(|| Some(DEFAULT_RANGE.to_string())),
        last_checked_at: Some(Utc::now()),
        message: if sheets_configured {
            Some("Google Sheets server-side sync is configured".to_string())
        } else {
            Some("Set GOOGLE_WORKSPACE_CLIENT_ID, GOOGLE_WORKSPACE_CLIENT_SECRET, GOOGLE_WORKSPACE_REFRESH_TOKEN, and PORCHFEST_GOOGLE_SHEET_ID on the backend".to_string())
        },
    }
}

fn clean_or_default(value: Option<String>, default_value: &str) -> String {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| default_value.to_string())
}

fn env_present(name: &str) -> bool {
    env::var(name)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false)
}

#[derive(Deserialize)]
struct GoogleTokenResponse {
    access_token: Option<String>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn google_access_token() -> async_graphql::Result<String> {
    let client_id = required_env("GOOGLE_WORKSPACE_CLIENT_ID")?;
    let client_secret = required_env("GOOGLE_WORKSPACE_CLIENT_SECRET")?;
    let refresh_token = required_env("GOOGLE_WORKSPACE_REFRESH_TOKEN")?;
    let client = Client::new();
    let response = client
        .post(GOOGLE_TOKEN_URL)
        .form(&[
            ("client_id", client_id),
            ("client_secret", client_secret),
            ("refresh_token", refresh_token),
            ("grant_type", "refresh_token".to_string()),
        ])
        .send()
        .await
        .map_err(|error| {
            async_graphql::Error::new(format!("Google token refresh failed: {error}"))
        })?;
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<unreadable token response>".to_string());
    let parsed: GoogleTokenResponse = serde_json::from_str(&body).map_err(|error| {
        async_graphql::Error::new(format!("Google token response was invalid JSON: {error}"))
    })?;
    if !status.is_success() {
        let message = parsed.error_description.or(parsed.error).unwrap_or(body);
        return Err(async_graphql::Error::new(format!(
            "Google token refresh failed: {message}"
        )));
    }
    parsed
        .access_token
        .filter(|token| !token.trim().is_empty())
        .ok_or_else(|| {
            async_graphql::Error::new("Google token response did not include an access token")
        })
}

fn required_env(name: &str) -> async_graphql::Result<String> {
    env::var(name)
        .map(|value| value.trim().to_string())
        .ok()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| async_graphql::Error::new(format!("{name} is not configured")))
}

#[derive(Deserialize)]
struct SheetValuesResponse {
    values: Option<Vec<Vec<Value>>>,
}

async fn fetch_sheet_rows(
    token: &str,
    spreadsheet_id: &str,
    range: &str,
    target_kind: &str,
) -> async_graphql::Result<Vec<GoogleSheetCivicRow>> {
    let url = format!(
        "{}/{}/values/{}?majorDimension=ROWS",
        GOOGLE_SHEETS_BASE,
        encode_component(spreadsheet_id),
        encode_component(range),
    );
    let response = Client::new()
        .get(url)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|error| {
            async_graphql::Error::new(format!("Google Sheets read failed: {error}"))
        })?;
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<unreadable sheets response>".to_string());
    if !status.is_success() {
        return Err(async_graphql::Error::new(format!(
            "Google Sheets read failed: {status} {body}"
        )));
    }
    let parsed: SheetValuesResponse = serde_json::from_str(&body).map_err(|error| {
        async_graphql::Error::new(format!("Google Sheets response was invalid JSON: {error}"))
    })?;
    Ok(parse_sheet_rows(
        spreadsheet_id,
        range,
        target_kind,
        parsed.values.unwrap_or_default(),
    ))
}

fn parse_sheet_rows(
    spreadsheet_id: &str,
    range: &str,
    target_kind: &str,
    values: Vec<Vec<Value>>,
) -> Vec<GoogleSheetCivicRow> {
    let Some(header_row) = values.first() else {
        return Vec::new();
    };
    let headers: Vec<String> = header_row.iter().map(cell_to_string).collect();
    values
        .into_iter()
        .enumerate()
        .skip(1)
        .filter_map(|(index, row)| {
            let mut fields = serde_json::Map::new();
            for (col, value) in row.iter().enumerate() {
                let Some(header) = headers.get(col) else {
                    continue;
                };
                let Some(field) = field_key_for_header(header) else {
                    continue;
                };
                let text = cell_to_string(value);
                if text.trim().is_empty() {
                    continue;
                }
                fields.insert(field.to_string(), Value::String(text));
            }
            if fields.is_empty() {
                return None;
            }
            if !fields.contains_key("category") {
                fields.insert("category".to_string(), Value::String("other".to_string()));
            }
            let row_number = index as i32 + 1;
            let source_id = fields
                .get("sourceId")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_string)
                .or_else(|| {
                    let category = fields
                        .get("category")
                        .and_then(Value::as_str)
                        .unwrap_or("other");
                    fields.get("email").and_then(Value::as_str).map(|email| {
                        format!("google-sheet:{category}:{}", email.trim().to_lowercase())
                    })
                })
                .unwrap_or_else(|| {
                    format!("google-sheet:{spreadsheet_id}:{range}:row-{row_number}")
                });
            fields.insert("sourceId".to_string(), Value::String(source_id.clone()));
            let payload = Value::Object(fields);
            let row_hash = stable_hash(&payload);
            Some(GoogleSheetCivicRow {
                source_id,
                row_number,
                target_kind: target_kind.to_string(),
                fields: Json(payload),
                row_hash,
            })
        })
        .collect()
}

fn field_key_for_header(header: &str) -> Option<&'static str> {
    let normalized: String = header
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    match normalized.as_str() {
        "sourceid" | "sourcekey" | "provenance" => Some("sourceId"),
        "category" | "type" | "applicationtype" => Some("category"),
        "name" | "contact" | "contactname" => Some("name"),
        "email" | "contactemail" | "emailaddress" => Some("email"),
        "phone" | "contactphone" | "phonenumber" => Some("phone"),
        "city" | "locationcity" => Some("city"),
        "bio" | "description" | "artistbio" => Some("bio"),
        "flintbased" | "flintconnection" => Some("flintBased"),
        "accessneeds" | "accessibilityneeds" | "setupneeds" => Some("accessNeeds"),
        "submittedat" | "submitted" | "timestamp" | "createdat" => Some("submittedAt"),
        "artistname" | "artistband" | "bandname" => Some("artistName"),
        "genre" | "musicgenre" => Some("genre"),
        "musiclink" | "samplelink" => Some("musicLink"),
        "businessname" | "vendorname" => Some("businessName"),
        "fooddescription" | "whatyouserve" | "items" => Some("foodDescription"),
        "foodtype" => Some("foodType"),
        "vendorlink" | "website" => Some("vendorLink"),
        "vendorneeds" | "onsiteneeds" => Some("vendorNeeds"),
        "actname" => Some("actName"),
        "acttype" => Some("actType"),
        "actdescription" => Some("actDescription"),
        "orgname" | "organization" | "organizationname" => Some("orgName"),
        "proposal" => Some("proposal"),
        "tier" | "sponsorshiplevel" => Some("tier"),
        "tierprice" | "sponsorshipamount" | "amount" => Some("tierPrice"),
        "sponsoringas" | "sponsorname" => Some("sponsoringAs"),
        _ => None,
    }
}

fn cell_to_string(value: &Value) -> String {
    match value {
        Value::String(value) => value.trim().to_string(),
        Value::Number(value) => value.to_string(),
        Value::Bool(value) => {
            if *value {
                "yes".to_string()
            } else {
                "no".to_string()
            }
        }
        _ => String::new(),
    }
}

fn export_values(rows: &[GoogleSheetCivicRowInput]) -> Vec<Vec<String>> {
    let mut keys: BTreeSet<String> = BTreeSet::new();
    for row in rows {
        if row
            .source_id
            .as_deref()
            .map(|value| !value.trim().is_empty())
            .unwrap_or(false)
        {
            keys.insert("sourceId".to_string());
        }
        if let Some(object) = row.fields.0.as_object() {
            for key in object.keys() {
                keys.insert(key.clone());
            }
        }
    }
    let mut headers: Vec<String> = PREFERRED_EXPORT_HEADERS
        .iter()
        .filter(|key| keys.contains(**key))
        .map(|key| (*key).to_string())
        .collect();
    for key in keys {
        if !headers.contains(&key) {
            headers.push(key);
        }
    }
    if headers.is_empty() {
        headers.extend(
            PREFERRED_EXPORT_HEADERS
                .iter()
                .map(|key| (*key).to_string()),
        );
    }

    let mut values = vec![headers.clone()];
    for row in rows {
        let object = row.fields.0.as_object();
        values.push(
            headers
                .iter()
                .map(|key| {
                    if key == "sourceId" {
                        row.source_id
                            .as_deref()
                            .or_else(|| {
                                object
                                    .and_then(|object| object.get(key))
                                    .and_then(Value::as_str)
                            })
                            .unwrap_or_default()
                            .to_string()
                    } else {
                        object
                            .and_then(|object| object.get(key))
                            .map(cell_to_string)
                            .unwrap_or_default()
                    }
                })
                .collect(),
        );
    }
    values
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SheetUpdateResponse {
    updated_range: Option<String>,
    updated_cells: Option<i32>,
}

async fn update_sheet_values(
    token: &str,
    spreadsheet_id: &str,
    range: &str,
    values: Vec<Vec<String>>,
) -> async_graphql::Result<SheetUpdateResponse> {
    let url = format!(
        "{}/{}/values/{}?valueInputOption=USER_ENTERED",
        GOOGLE_SHEETS_BASE,
        encode_component(spreadsheet_id),
        encode_component(range),
    );
    let response = Client::new()
        .put(url)
        .bearer_auth(token)
        .json(&json!({
            "range": range,
            "majorDimension": "ROWS",
            "values": values,
        }))
        .send()
        .await
        .map_err(|error| {
            async_graphql::Error::new(format!("Google Sheets export failed: {error}"))
        })?;
    let status = response.status();
    let body = response
        .text()
        .await
        .unwrap_or_else(|_| "<unreadable sheets update response>".to_string());
    if !status.is_success() {
        return Err(async_graphql::Error::new(format!(
            "Google Sheets export failed: {status} {body}"
        )));
    }
    serde_json::from_str(&body).map_err(|error| {
        async_graphql::Error::new(format!(
            "Google Sheets export response was invalid JSON: {error}"
        ))
    })
}

fn encode_component(value: &str) -> String {
    let mut out = String::new();
    for byte in value.as_bytes() {
        match *byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(char::from(*byte));
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn stable_hash(value: &Value) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(value.to_string());
    format!("{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_sheet_headers_to_civic_fields() {
        assert_eq!(field_key_for_header("Email Address"), Some("email"));
        assert_eq!(field_key_for_header("Business Name"), Some("businessName"));
        assert_eq!(
            field_key_for_header("Sponsorship Amount"),
            Some("tierPrice")
        );
        assert_eq!(field_key_for_header("Notes nobody imports"), None);
    }

    #[test]
    fn parses_sheet_rows_with_stable_source_ids() {
        let rows = parse_sheet_rows(
            "spreadsheet-1",
            "Applications!A:Z",
            "applications",
            vec![
                vec![
                    json!("Timestamp"),
                    json!("Email Address"),
                    json!("Business Name"),
                    json!("Food Description"),
                ],
                vec![
                    json!("2026-06-15T10:00:00Z"),
                    json!("Ray@Example.com"),
                    json!("Coney Ray"),
                    json!("Flint-style coneys"),
                ],
            ],
        );

        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.source_id, "google-sheet:other:ray@example.com");
        assert_eq!(row.row_number, 2);
        assert_eq!(row.target_kind, "applications");
        assert_eq!(row.fields.0["email"], json!("Ray@Example.com"));
        assert_eq!(row.fields.0["businessName"], json!("Coney Ray"));
        assert_eq!(row.fields.0["category"], json!("other"));
        assert_eq!(row.row_hash.len(), 64);
    }

    #[test]
    fn export_values_keeps_preferred_headers_first() {
        let values = export_values(&[GoogleSheetCivicRowInput {
            source_id: Some("google-sheet:sponsor:acme@example.com".to_string()),
            fields: Json(json!({
                "email": "acme@example.com",
                "tierPrice": "5000",
                "orgName": "A.C.M.E. Foundation",
                "customColumn": "kept",
            })),
        }]);

        assert_eq!(values[0][0], "sourceId");
        assert!(values[0].contains(&"email".to_string()));
        assert!(values[0].contains(&"orgName".to_string()));
        assert!(values[0].contains(&"tierPrice".to_string()));
        assert!(values[0].contains(&"customColumn".to_string()));
        assert_eq!(values[1][0], "google-sheet:sponsor:acme@example.com");
    }

    #[test]
    fn encodes_google_path_components() {
        assert_eq!(encode_component("Applications!A:Z"), "Applications%21A%3AZ");
        assert_eq!(encode_component("sheet id"), "sheet%20id");
    }
}
