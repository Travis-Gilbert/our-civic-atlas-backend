//! Minimal glTF 2.0 binary (GLB) writer.
//!
//! Authors one GLB from a `MassingModel`: one mesh and node per part, node
//! names carrying the stable reconstruction-node-tree ids, node extras
//! carrying the per-part provenance flag, and PBR materials deduped by
//! name. The GLB path in the frontend (deck.gl ScenegraphLayer) bypasses
//! the procedural confidence shader, so this is where documented-versus-
//! inferred gets baked into the artifact itself.
//!
//! Container layout per the glTF 2.0 spec: 12-byte header (magic `glTF`,
//! version 2, total length), a JSON chunk padded to 4 bytes with spaces,
//! and a BIN chunk padded with zeros.

use std::collections::BTreeMap;

use anyhow::Result;
use serde_json::{json, Value};

use civic_atlas_reconstruction_engine::reconstruction_node_id;
use civic_atlas_types::civic_atlas::v1::ReconstructionSpec;

use crate::massing::MassingModel;
use crate::tier::TierDecision;

const GLB_MAGIC: u32 = 0x4654_6C67; // "glTF"
const CHUNK_JSON: u32 = 0x4E4F_534A; // "JSON"
const CHUNK_BIN: u32 = 0x004E_4942; // "BIN\0"

const COMPONENT_F32: u32 = 5126;
const COMPONENT_U32: u32 = 5125;
const TARGET_ARRAY_BUFFER: u32 = 34962;
const TARGET_ELEMENT_ARRAY_BUFFER: u32 = 34963;

pub const GLB_GENERATOR: &str = concat!("civic-atlas-renderer/", env!("CARGO_PKG_VERSION"));

fn pad_to_4(buffer: &mut Vec<u8>, pad: u8) {
    while buffer.len() % 4 != 0 {
        buffer.push(pad);
    }
}

/// Write the massing model as a GLB byte vector.
pub fn write_glb(
    model: &MassingModel,
    spec: &ReconstructionSpec,
    decision: &TierDecision,
) -> Result<Vec<u8>> {
    let mut bin: Vec<u8> = Vec::new();
    let mut accessors: Vec<Value> = Vec::new();
    let mut buffer_views: Vec<Value> = Vec::new();
    let mut meshes: Vec<Value> = Vec::new();
    let mut nodes: Vec<Value> = Vec::new();
    let mut materials: Vec<Value> = Vec::new();
    let mut material_index: BTreeMap<String, usize> = BTreeMap::new();

    let mut part_node_indices: Vec<usize> = Vec::new();

    for part in &model.parts {
        // Material (deduped by name).
        let material_id = *material_index.entry(part.material_name.clone()).or_insert_with(|| {
            let index = materials.len();
            materials.push(json!({
                "name": part.material_name,
                "doubleSided": true,
                "pbrMetallicRoughness": {
                    "baseColorFactor": part.base_color,
                    "metallicFactor": 0.0,
                    "roughnessFactor": 0.72
                }
            }));
            index
        });

        // Vertex data: positions then normals, one ARRAY_BUFFER view each.
        let vertex_count = part.positions.len();
        anyhow::ensure!(vertex_count > 0, "empty part {}", part.field_path);
        anyhow::ensure!(
            part.normals.len() == vertex_count,
            "normal count mismatch on {}",
            part.field_path
        );

        let mut min = [f32::MAX; 3];
        let mut max = [f32::MIN; 3];
        for position in &part.positions {
            for axis in 0..3 {
                min[axis] = min[axis].min(position[axis]);
                max[axis] = max[axis].max(position[axis]);
            }
        }

        pad_to_4(&mut bin, 0);
        let position_offset = bin.len();
        for position in &part.positions {
            for component in position {
                bin.extend_from_slice(&component.to_le_bytes());
            }
        }
        let position_view = buffer_views.len();
        buffer_views.push(json!({
            "buffer": 0,
            "byteOffset": position_offset,
            "byteLength": vertex_count * 12,
            "target": TARGET_ARRAY_BUFFER
        }));
        let position_accessor = accessors.len();
        accessors.push(json!({
            "bufferView": position_view,
            "componentType": COMPONENT_F32,
            "count": vertex_count,
            "type": "VEC3",
            "min": min,
            "max": max
        }));

        pad_to_4(&mut bin, 0);
        let normal_offset = bin.len();
        for normal in &part.normals {
            for component in normal {
                bin.extend_from_slice(&component.to_le_bytes());
            }
        }
        let normal_view = buffer_views.len();
        buffer_views.push(json!({
            "buffer": 0,
            "byteOffset": normal_offset,
            "byteLength": vertex_count * 12,
            "target": TARGET_ARRAY_BUFFER
        }));
        let normal_accessor = accessors.len();
        accessors.push(json!({
            "bufferView": normal_view,
            "componentType": COMPONENT_F32,
            "count": vertex_count,
            "type": "VEC3"
        }));

        pad_to_4(&mut bin, 0);
        let index_offset = bin.len();
        for index in &part.indices {
            bin.extend_from_slice(&index.to_le_bytes());
        }
        let index_view = buffer_views.len();
        buffer_views.push(json!({
            "buffer": 0,
            "byteOffset": index_offset,
            "byteLength": part.indices.len() * 4,
            "target": TARGET_ELEMENT_ARRAY_BUFFER
        }));
        let index_accessor = accessors.len();
        accessors.push(json!({
            "bufferView": index_view,
            "componentType": COMPONENT_U32,
            "count": part.indices.len(),
            "type": "SCALAR"
        }));

        let mesh_id = meshes.len();
        meshes.push(json!({
            "name": format!("{} mesh", part.label),
            "primitives": [{
                "attributes": { "POSITION": position_accessor, "NORMAL": normal_accessor },
                "indices": index_accessor,
                "material": material_id,
                "mode": 4
            }]
        }));

        let node_id = nodes.len();
        nodes.push(json!({
            "name": reconstruction_node_id(spec, &part.field_path),
            "mesh": mesh_id,
            "extras": {
                "fieldPath": part.field_path,
                "label": part.label,
                "provenance": serde_json::to_value(&part.flag)?
            }
        }));
        part_node_indices.push(node_id);
    }

    // Root building node: carries the spec identity and the tier decision so
    // the asset is self-describing even outside the manifest.
    let root_index = nodes.len();
    nodes.push(json!({
        "name": reconstruction_node_id(spec, "building"),
        "children": part_node_indices,
        "extras": {
            "specId": spec.spec_id,
            "specVersion": spec.spec_version,
            "renderTier": decision.tier.as_str(),
            "renderTierRationale": decision.rationale,
            "roofForm": model.roof_form.as_str(),
            "dimensionsMeters": {
                "width": model.dims.width,
                "depth": model.dims.depth,
                "height": model.dims.height,
                "stories": model.dims.stories
            }
        }
    }));

    pad_to_4(&mut bin, 0);
    let gltf = json!({
        "asset": {
            "version": "2.0",
            "generator": GLB_GENERATOR
        },
        "scene": 0,
        "scenes": [{ "name": spec.spec_id, "nodes": [root_index] }],
        "nodes": nodes,
        "meshes": meshes,
        "materials": materials,
        "accessors": accessors,
        "bufferViews": buffer_views,
        "buffers": [{ "byteLength": bin.len() }]
    });

    let mut json_bytes = serde_json::to_vec(&gltf)?;
    pad_to_4(&mut json_bytes, b' ');

    let total_length = 12 + 8 + json_bytes.len() + 8 + bin.len();
    let mut glb = Vec::with_capacity(total_length);
    glb.extend_from_slice(&GLB_MAGIC.to_le_bytes());
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total_length as u32).to_le_bytes());
    glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
    glb.extend_from_slice(&CHUNK_JSON.to_le_bytes());
    glb.extend_from_slice(&json_bytes);
    glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
    glb.extend_from_slice(&CHUNK_BIN.to_le_bytes());
    glb.extend_from_slice(&bin);

    Ok(glb)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::massing::build_massing;
    use crate::tier::{select_tier, TierThresholds};
    use civic_atlas_types::civic_atlas::v1::{
        DimensionRange, Facade, Mass, OpeningGrid, PartProvenance, ReconstructionSource,
        ReconstructionSourceType, Roof,
    };

    fn test_spec() -> ReconstructionSpec {
        let provenance = PartProvenance {
            sources: vec![ReconstructionSource {
                source_id: "sanborn-1".to_string(),
                source_type: ReconstructionSourceType::Map as i32,
                ..Default::default()
            }],
            part_confidence: 0.9,
            ..Default::default()
        };
        ReconstructionSpec {
            spec_id: "recon-whaley-1900".to_string(),
            spec_version: 1,
            mass: Some(Mass {
                provenance: Some(provenance.clone()),
                stories: 2,
                height: Some(DimensionRange { min: Some(7.0), max: Some(7.0), unit: "m".to_string() }),
                width: Some(DimensionRange { min: Some(10.0), max: Some(10.0), unit: "m".to_string() }),
                depth: Some(DimensionRange { min: Some(14.0), max: Some(14.0), unit: "m".to_string() }),
                ..Default::default()
            }),
            facades: vec![Facade {
                provenance: Some(provenance.clone()),
                facade_side: "front".to_string(),
                primary_material: "brick".to_string(),
                opening_grids: vec![OpeningGrid {
                    provenance: Some(provenance.clone()),
                    bay_count: 3,
                    floor_count: 2,
                    ..Default::default()
                }],
                ..Default::default()
            }],
            roof: Some(Roof {
                provenance: Some(provenance),
                roof_type: "gable".to_string(),
                roof_material: "slate".to_string(),
                ..Default::default()
            }),
            ..Default::default()
        }
    }

    #[test]
    fn glb_parses_with_reference_loader_and_carries_provenance() {
        let spec = test_spec();
        let decision = select_tier(&spec, &TierThresholds::default());
        let model = build_massing(&spec).unwrap();
        let bytes = write_glb(&model, &spec, &decision).unwrap();

        // Header sanity.
        assert_eq!(&bytes[0..4], b"glTF");
        assert_eq!(bytes.len() % 4, 0);

        // Reference-loader round trip: if this parses, accessors, buffer
        // views, alignment, and min/max are structurally valid.
        let glb = gltf::Glb::from_slice(&bytes).expect("GLB container parses");
        let document = gltf::Gltf::from_slice(&bytes).expect("glTF JSON validates");

        let bin = glb.bin.expect("BIN chunk present");
        assert!(!bin.is_empty());

        // Every part node carries provenance extras; ghost walls are flagged.
        let mut ghost_walls = 0;
        let mut documented_parts = 0;
        for node in document.nodes() {
            let Some(extras) = node.extras().as_ref() else { continue };
            let extras: serde_json::Value = serde_json::from_str(extras.get()).unwrap();
            if let Some(provenance) = extras.get("provenance") {
                if provenance["documented"].as_bool() == Some(true) {
                    documented_parts += 1;
                } else {
                    ghost_walls += 1;
                }
            }
            if let Some(name) = node.name() {
                if extras.get("provenance").is_some() {
                    assert!(
                        name.starts_with("reconstruction-node:"),
                        "part node names use the node-tree id namespace, got {name}"
                    );
                }
            }
        }
        assert!(documented_parts >= 3, "front facade + grid + roof + mass documented");
        assert!(ghost_walls >= 3, "three undocumented walls render flagged");

        // Mesh geometry is non-degenerate: positions exist and span meters.
        let buffers = gltf::import_slice(&bytes).expect("full import");
        let (_, buffer_data, _) = buffers;
        let primitive = document
            .meshes()
            .next()
            .unwrap()
            .primitives()
            .next()
            .unwrap();
        let reader = primitive.reader(|buffer| Some(&buffer_data[buffer.index()]));
        let positions: Vec<[f32; 3]> = reader.read_positions().unwrap().collect();
        assert!(!positions.is_empty());
    }
}
