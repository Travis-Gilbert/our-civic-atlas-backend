use std::{fs, path::Path};

use civic_atlas_types::civic_atlas::v1::CivicObject;
use serde_json::Value;

pub fn load_places_from_geojson(
    path: impl AsRef<Path>,
    tenant_id: &str,
) -> anyhow::Result<Vec<CivicObject>> {
    let raw = fs::read_to_string(path)?;
    let value: Value = serde_json::from_str(&raw)?;
    let features = value
        .get("features")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut places = Vec::with_capacity(features.len());
    for feature in features {
        let properties = feature.get("properties").cloned().unwrap_or(Value::Null);
        let place_id = properties
            .get("place_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if place_id.is_empty() {
            continue;
        }

        let name = properties
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(place_id);
        let object_type = properties
            .get("place_type")
            .and_then(Value::as_str)
            .unwrap_or("place");
        let source_ids = properties
            .get("source_ids")
            .and_then(Value::as_array)
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let geometry_json = feature
            .get("geometry")
            .map(Value::to_string)
            .unwrap_or_else(|| "null".to_string());

        places.push(CivicObject {
            id: place_id.to_string(),
            tenant_id: tenant_id.to_string(),
            name: name.to_string(),
            object_type: object_type.to_string(),
            geometry_json,
            time_start_ms: None,
            time_end_ms: None,
            confidence: 1.0,
            source_ids,
            dossier_path: format!("/open-flint-atlas/place/{}", url_escape(place_id)),
            attributes: Default::default(),
        });
    }

    Ok(places)
}

pub fn seed_places(tenant_id: &str) -> Vec<CivicObject> {
    ["place:flint", "place:carriage-town", "ward:1"]
        .into_iter()
        .map(|id| CivicObject {
            id: id.to_string(),
            tenant_id: tenant_id.to_string(),
            name: id.replace([':', '-'], " "),
            object_type: "place".to_string(),
            geometry_json: "null".to_string(),
            time_start_ms: None,
            time_end_ms: None,
            confidence: 0.5,
            source_ids: Vec::new(),
            dossier_path: format!("/open-flint-atlas/place/{}", url_escape(id)),
            attributes: Default::default(),
        })
        .collect()
}

fn url_escape(value: &str) -> String {
    value.replace(':', "%3A")
}
