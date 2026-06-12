//! Documented-versus-inferred classification and provenance write-through.
//!
//! The spec already records when a part is not documented and carries a
//! confidence per part. The renderer carries that through: documented parts
//! render plainly in their material color, inferred parts render in the
//! ghost porcelain palette, and every produced part is queryable through the
//! provenance record asset and the GLB node extras. A rendered building that
//! looks confident and is actually inference is the failure to avoid.

use serde::{Deserialize, Serialize};

use civic_atlas_types::civic_atlas::v1::{
    PartProvenance, ReconstructionSourceType, ReconstructionSpec, TextureProvenance,
};

use crate::tier::{RenderTier, TierDecision};

/// The frontend's ghost porcelain palette (GHOST_PALETTE in
/// `historical-reconstruction.ts`). The GLB path bypasses the procedural
/// confidence-mix shader, so the same palette is baked into inferred-part
/// materials here to keep the visual grammar consistent across both paths.
pub mod ghost_palette {
    /// #F2F8F7 - openings on inferred parts.
    pub const HIGHLIGHT: [f32; 4] = [0.949, 0.973, 0.969, 1.0];
    /// #CFE0DC - inferred roof surfaces.
    pub const MID: [f32; 4] = [0.812, 0.878, 0.863, 1.0];
    /// #9CC0B8 - inferred walls (matches the shader's porcelain substitution).
    pub const SHADOW: [f32; 4] = [0.612, 0.753, 0.722, 1.0];
}

/// How a produced part presents: plainly (documented) or flagged (inference).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MaterialTreatment {
    Documented,
    GhostInferred,
}

/// Per-part provenance flag derived from the spec's `PartProvenance`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartFlag {
    pub documented: bool,
    pub confidence: f64,
    pub from_gnn_prior: bool,
    pub source_ids: Vec<String>,
    pub source_types: Vec<String>,
    pub treatment: MaterialTreatment,
}

impl PartFlag {
    /// A part with no provenance at all is an undocumented part: rendered,
    /// but flagged as inference with zero confidence. The render never makes
    /// a guess look like a fact.
    pub fn undocumented() -> Self {
        Self {
            documented: false,
            confidence: 0.0,
            from_gnn_prior: false,
            source_ids: Vec::new(),
            source_types: Vec::new(),
            treatment: MaterialTreatment::GhostInferred,
        }
    }
}

fn source_type_name(value: i32) -> &'static str {
    match ReconstructionSourceType::try_from(value) {
        Ok(ReconstructionSourceType::ArchivalPhoto) => "archival_photo",
        Ok(ReconstructionSourceType::Map) => "map",
        Ok(ReconstructionSourceType::Permit) => "permit",
        Ok(ReconstructionSourceType::Survey) => "survey",
        Ok(ReconstructionSourceType::OralHistory) => "oral_history",
        Ok(ReconstructionSourceType::ModelPrior) => "model_prior",
        Ok(ReconstructionSourceType::Other) => "other",
        _ => "unspecified",
    }
}

/// True when the source type is real recorded evidence rather than a model
/// prior or a system fallback.
fn is_evidence_source(value: i32) -> bool {
    matches!(
        ReconstructionSourceType::try_from(value),
        Ok(ReconstructionSourceType::ArchivalPhoto)
            | Ok(ReconstructionSourceType::Map)
            | Ok(ReconstructionSourceType::Permit)
            | Ok(ReconstructionSourceType::Survey)
            | Ok(ReconstructionSourceType::OralHistory)
    )
}

/// Classify a part. Documented means: not produced by the GNN/neighbor
/// prior, and at least one of its sources is recorded evidence (photo, map,
/// permit, survey, oral history). The engine's footprint fallback
/// (`system_provenance`, source type Other) and the block-coherent prior
/// (`prior_provenance`, ModelPrior + from_gnn_prior) both classify as
/// inference.
pub fn classify_part(provenance: Option<&PartProvenance>) -> PartFlag {
    let Some(provenance) = provenance else {
        return PartFlag::undocumented();
    };
    let has_evidence = provenance
        .sources
        .iter()
        .any(|source| is_evidence_source(source.source_type));
    let documented = has_evidence && !provenance.from_gnn_prior;
    PartFlag {
        documented,
        confidence: provenance.part_confidence,
        from_gnn_prior: provenance.from_gnn_prior,
        source_ids: provenance
            .sources
            .iter()
            .map(|source| source.source_id.clone())
            .collect(),
        source_types: provenance
            .sources
            .iter()
            .map(|source| source_type_name(source.source_type).to_string())
            .collect(),
        treatment: if documented {
            MaterialTreatment::Documented
        } else {
            MaterialTreatment::GhostInferred
        },
    }
}

/// One row in the provenance record asset: a produced part, its node-tree
/// address, and how it presents.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PartProvenanceRecord {
    pub node_id: String,
    pub part_type: String,
    pub field_path: String,
    #[serde(flatten)]
    pub flag: PartFlag,
}

/// The provenance record document written next to the geometry asset. This
/// is what `foundryAssetUrl` points at: a queryable, per-part account of
/// documented-versus-inferred for everything the renderer produced.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProvenanceRecord {
    pub spec_id: String,
    pub spec_version: u32,
    pub render_tier: String,
    pub render_tier_rationale: String,
    pub generator: String,
    pub parts: Vec<PartProvenanceRecord>,
    pub texture: Vec<TextureProvenanceRecord>,
    pub photo_sources: Vec<PhotoSourceRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureProvenanceRecord {
    pub node_id: String,
    pub texture_source: String,
    pub texture_confidence: Option<f64>,
    pub lora_archetype: Option<String>,
    pub controlnet_conditioning_source: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PhotoSourceRecord {
    pub source_id: String,
    pub title: String,
    pub uri: String,
}

/// Texture source written for the synchronous massing render. The massing
/// GLB carries flat PBR colors, never a fitted photo texture, so the honest
/// value is procedural; the GPU appearance stage upgrades it to
/// `archival_photo` when a fitted texture actually lands.
pub const TEXTURE_SOURCE_PROCEDURAL: &str = "procedural_pbr";
/// Texture source recorded by the tier A appearance stage (rectified,
/// inpainted archival photo projected onto the massing faces).
pub const TEXTURE_SOURCE_ARCHIVAL: &str = "archival_photo";
/// Flagged last resort: generated texture (LoRA / diffusion). Never the
/// primary appearance source.
pub const TEXTURE_SOURCE_GENERATED: &str = "lora_only";

/// Stamp `TextureProvenance` onto every facade and the roof for the
/// synchronous massing render. Existing texture provenance (for example a
/// completed GPU appearance pass recorded on a stored spec) is left intact;
/// only missing provenance is filled, so re-running the renderer never
/// downgrades an archival texture record to procedural.
pub fn stamp_massing_texture_provenance(spec: &mut ReconstructionSpec, tier: RenderTier) {
    let confidence_scale = match tier {
        RenderTier::DescriptionOnly => 0.35,
        _ => 0.45,
    };
    for facade in &mut spec.facades {
        if facade.texture_provenance.is_none() {
            let part_confidence = facade
                .provenance
                .as_ref()
                .map(|provenance| provenance.part_confidence)
                .unwrap_or(0.0);
            facade.texture_provenance = Some(TextureProvenance {
                texture_source: TEXTURE_SOURCE_PROCEDURAL.to_string(),
                texture_confidence: Some((part_confidence * confidence_scale).max(0.05)),
                ..Default::default()
            });
        }
    }
    if let Some(roof) = spec.roof.as_mut() {
        if roof.texture_provenance.is_none() {
            let part_confidence = roof
                .provenance
                .as_ref()
                .map(|provenance| provenance.part_confidence)
                .unwrap_or(0.0);
            roof.texture_provenance = Some(TextureProvenance {
                texture_source: TEXTURE_SOURCE_PROCEDURAL.to_string(),
                texture_confidence: Some((part_confidence * confidence_scale).max(0.05)),
                ..Default::default()
            });
        }
    }
}

/// Build the texture section of the provenance record from the (stamped)
/// spec.
pub fn texture_records(spec: &ReconstructionSpec) -> Vec<TextureProvenanceRecord> {
    let mut records = Vec::new();
    for (index, facade) in spec.facades.iter().enumerate() {
        if let Some(texture) = facade.texture_provenance.as_ref() {
            records.push(TextureProvenanceRecord {
                node_id: civic_atlas_reconstruction_engine::reconstruction_node_id(
                    spec,
                    &format!("facades[{index}]"),
                ),
                texture_source: texture.texture_source.clone(),
                texture_confidence: texture.texture_confidence,
                lora_archetype: (!texture.lora_archetype.is_empty())
                    .then(|| texture.lora_archetype.clone()),
                controlnet_conditioning_source: (!texture.controlnet_conditioning_source.is_empty())
                    .then(|| texture.controlnet_conditioning_source.clone()),
            });
        }
    }
    if let Some(texture) = spec.roof.as_ref().and_then(|roof| roof.texture_provenance.as_ref()) {
        records.push(TextureProvenanceRecord {
            node_id: civic_atlas_reconstruction_engine::reconstruction_node_id(spec, "roof"),
            texture_source: texture.texture_source.clone(),
            texture_confidence: texture.texture_confidence,
            lora_archetype: (!texture.lora_archetype.is_empty())
                .then(|| texture.lora_archetype.clone()),
            controlnet_conditioning_source: (!texture.controlnet_conditioning_source.is_empty())
                .then(|| texture.controlnet_conditioning_source.clone()),
        });
    }
    records
}

/// Build the photo-source section of the provenance record from the tier
/// decision.
pub fn photo_source_records(decision: &TierDecision) -> Vec<PhotoSourceRecord> {
    decision
        .photo_sources
        .iter()
        .map(|source| PhotoSourceRecord {
            source_id: source.source_id.clone(),
            title: source.title.clone(),
            uri: source.uri.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use civic_atlas_types::civic_atlas::v1::{Facade, ReconstructionSource, Roof};

    fn evidence_provenance(confidence: f64) -> PartProvenance {
        PartProvenance {
            sources: vec![ReconstructionSource {
                source_id: "sanborn-1".to_string(),
                source_type: ReconstructionSourceType::Map as i32,
                ..Default::default()
            }],
            part_confidence: confidence,
            from_gnn_prior: false,
            ..Default::default()
        }
    }

    fn prior_provenance(confidence: f64) -> PartProvenance {
        PartProvenance {
            sources: vec![ReconstructionSource {
                source_id: "model:test-v1".to_string(),
                source_type: ReconstructionSourceType::ModelPrior as i32,
                ..Default::default()
            }],
            part_confidence: confidence,
            from_gnn_prior: true,
            ..Default::default()
        }
    }

    fn system_fallback_provenance(confidence: f64) -> PartProvenance {
        PartProvenance {
            sources: vec![ReconstructionSource {
                source_id: "system:footprint".to_string(),
                source_type: ReconstructionSourceType::Other as i32,
                ..Default::default()
            }],
            part_confidence: confidence,
            from_gnn_prior: false,
            ..Default::default()
        }
    }

    #[test]
    fn evidence_part_is_documented() {
        let flag = classify_part(Some(&evidence_provenance(0.92)));
        assert!(flag.documented);
        assert_eq!(flag.treatment, MaterialTreatment::Documented);
        assert_eq!(flag.source_types, vec!["map"]);
    }

    #[test]
    fn prior_part_is_inferred() {
        let flag = classify_part(Some(&prior_provenance(0.54)));
        assert!(!flag.documented);
        assert_eq!(flag.treatment, MaterialTreatment::GhostInferred);
        assert!(flag.from_gnn_prior);
    }

    #[test]
    fn system_fallback_is_inferred() {
        let flag = classify_part(Some(&system_fallback_provenance(0.58)));
        assert!(!flag.documented);
        assert_eq!(flag.treatment, MaterialTreatment::GhostInferred);
    }

    #[test]
    fn missing_provenance_is_undocumented() {
        let flag = classify_part(None);
        assert!(!flag.documented);
        assert_eq!(flag.confidence, 0.0);
    }

    #[test]
    fn stamping_fills_only_missing_texture_provenance() {
        let mut spec = ReconstructionSpec {
            spec_id: "recon-test".to_string(),
            facades: vec![
                Facade {
                    provenance: Some(evidence_provenance(0.8)),
                    texture_provenance: Some(TextureProvenance {
                        texture_source: TEXTURE_SOURCE_ARCHIVAL.to_string(),
                        texture_confidence: Some(0.77),
                        ..Default::default()
                    }),
                    ..Default::default()
                },
                Facade {
                    provenance: Some(prior_provenance(0.51)),
                    ..Default::default()
                },
            ],
            roof: Some(Roof {
                provenance: Some(prior_provenance(0.47)),
                ..Default::default()
            }),
            ..Default::default()
        };
        stamp_massing_texture_provenance(&mut spec, RenderTier::DescriptionOnly);

        let kept = spec.facades[0].texture_provenance.as_ref().unwrap();
        assert_eq!(kept.texture_source, TEXTURE_SOURCE_ARCHIVAL);
        assert_eq!(kept.texture_confidence, Some(0.77));

        let stamped = spec.facades[1].texture_provenance.as_ref().unwrap();
        assert_eq!(stamped.texture_source, TEXTURE_SOURCE_PROCEDURAL);
        assert!(stamped.texture_confidence.unwrap() > 0.0);

        let roof = spec.roof.as_ref().unwrap().texture_provenance.as_ref().unwrap();
        assert_eq!(roof.texture_source, TEXTURE_SOURCE_PROCEDURAL);
    }
}
