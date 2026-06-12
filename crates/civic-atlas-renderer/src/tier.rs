//! Evidence-graded render tier selection.
//!
//! The amount and kind of photographic evidence selects the method, and the
//! method is honest about it. Photo evidence reaches the renderer through the
//! spec itself: every extracted part carries `PartProvenance.sources`, and an
//! archival photo that contributed to any part appears there as a
//! `ReconstructionSource` with `source_type == ArchivalPhoto` and the photo
//! `uri`. Counting distinct photo sources across all parts grades the
//! evidence without changing the `AssetGenerator` port.

use std::collections::BTreeMap;

use civic_atlas_types::civic_atlas::v1::{
    ReconstructionSource, ReconstructionSourceType, ReconstructionSpec,
};

/// Evidence-graded render tier. Each tier is more honest about what it knows
/// than the one below it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderTier {
    /// Tier A: exactly one facade photo. Inverse-procedural structure
    /// (facade parse) + rectify/inpaint/project appearance + PyTorch3D
    /// differentiable fitting on the GPU lane.
    SingleFacadePhoto,
    /// Tier B (sparse): a few uncalibrated photos. MASt3R / VGGT relative
    /// geometry -> Open3D Poisson mesh on the GPU lane.
    SparsePhotos,
    /// Tier B (many): enough photos for 2D Gaussian splatting -> Open3D
    /// Poisson mesh on the GPU lane.
    ManyPhotos,
    /// Tier C: footprint and a description, no photo. Procedural massing and
    /// facade from grammar defaults with period PBR materials, rendered
    /// ghosted to reflect low evidence.
    DescriptionOnly,
}

impl RenderTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            RenderTier::SingleFacadePhoto => "tier_a_single_facade_photo",
            RenderTier::SparsePhotos => "tier_b_sparse_photos",
            RenderTier::ManyPhotos => "tier_b_many_photos",
            RenderTier::DescriptionOnly => "tier_c_description_only",
        }
    }

    /// Whether this tier has a GPU refinement stage beyond the synchronous
    /// massing render.
    pub fn wants_refinement(&self) -> bool {
        true
    }

    /// The refinement job kind dispatched to the GPU lane.
    pub fn refinement_kind(&self) -> &'static str {
        match self {
            RenderTier::SingleFacadePhoto => "single_facade_fit",
            RenderTier::SparsePhotos => "sparse_multiview",
            RenderTier::ManyPhotos => "gaussian_splatting",
            RenderTier::DescriptionOnly => "procedural_archetype",
        }
    }
}

/// Photo-count thresholds for the tier ladder. Free to tune via env/config
/// without touching selection logic.
#[derive(Debug, Clone)]
pub struct TierThresholds {
    /// Minimum distinct photos for the many-photos (splatting) tier.
    pub many_photos_min: usize,
}

impl Default for TierThresholds {
    fn default() -> Self {
        Self {
            many_photos_min: 8,
        }
    }
}

/// The tier decision plus the evidence that produced it. Travels into the
/// manifest metadata and the refinement job payload.
#[derive(Debug, Clone)]
pub struct TierDecision {
    pub tier: RenderTier,
    /// Distinct archival-photo sources found on the spec, deduped by
    /// source_id, in first-seen order.
    pub photo_sources: Vec<ReconstructionSource>,
    pub rationale: String,
}

impl TierDecision {
    pub fn metadata(&self) -> BTreeMap<String, String> {
        let mut metadata = BTreeMap::new();
        metadata.insert("renderTier".to_string(), self.tier.as_str().to_string());
        metadata.insert(
            "renderTierRationale".to_string(),
            self.rationale.clone(),
        );
        metadata.insert(
            "photoSourceCount".to_string(),
            self.photo_sources.len().to_string(),
        );
        if !self.photo_sources.is_empty() {
            metadata.insert(
                "photoSourceIds".to_string(),
                self.photo_sources
                    .iter()
                    .map(|source| source.source_id.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            );
        }
        metadata
    }
}

/// Walk every part provenance on the spec and collect distinct archival
/// photo sources.
pub fn collect_photo_sources(spec: &ReconstructionSpec) -> Vec<ReconstructionSource> {
    let mut seen = std::collections::BTreeSet::new();
    let mut photos = Vec::new();
    let mut push = |sources: &[ReconstructionSource]| {
        for source in sources {
            if source.source_type == ReconstructionSourceType::ArchivalPhoto as i32
                && seen.insert(source.source_id.clone())
            {
                photos.push(source.clone());
            }
        }
    };

    if let Some(provenance) = spec.mass.as_ref().and_then(|mass| mass.provenance.as_ref()) {
        push(&provenance.sources);
    }
    for facade in &spec.facades {
        if let Some(provenance) = facade.provenance.as_ref() {
            push(&provenance.sources);
        }
        for grid in &facade.opening_grids {
            if let Some(provenance) = grid.provenance.as_ref() {
                push(&provenance.sources);
            }
            for opening_override in &grid.opening_overrides {
                if let Some(provenance) = opening_override.override_provenance.as_ref() {
                    push(&provenance.sources);
                }
            }
        }
    }
    if let Some(provenance) = spec.roof.as_ref().and_then(|roof| roof.provenance.as_ref()) {
        push(&provenance.sources);
    }
    if let Some(provenance) = spec
        .ground_floor
        .as_ref()
        .and_then(|ground| ground.provenance.as_ref())
    {
        push(&provenance.sources);
    }
    for ornament in &spec.ornaments {
        if let Some(provenance) = ornament.provenance.as_ref() {
            push(&provenance.sources);
        }
    }
    photos
}

/// Select the render tier from the photographic evidence on the spec.
pub fn select_tier(spec: &ReconstructionSpec, thresholds: &TierThresholds) -> TierDecision {
    let photo_sources = collect_photo_sources(spec);
    let count = photo_sources.len();
    let (tier, rationale) = match count {
        0 => (
            RenderTier::DescriptionOnly,
            "no archival photo sources on any part provenance; procedural massing from grammar defaults, rendered ghosted".to_string(),
        ),
        1 => (
            RenderTier::SingleFacadePhoto,
            "exactly one archival photo source; inverse-procedural facade parse plus rectify/inpaint/project appearance".to_string(),
        ),
        n if n >= thresholds.many_photos_min => (
            RenderTier::ManyPhotos,
            format!(
                "{n} archival photo sources (>= {}); 2D Gaussian splatting capture tier",
                thresholds.many_photos_min
            ),
        ),
        n => (
            RenderTier::SparsePhotos,
            format!("{n} sparse uncalibrated archival photo sources; MASt3R/VGGT relative geometry tier"),
        ),
    };
    TierDecision {
        tier,
        photo_sources,
        rationale,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use civic_atlas_types::civic_atlas::v1::{Facade, Mass, PartProvenance};

    fn photo_source(id: &str) -> ReconstructionSource {
        ReconstructionSource {
            source_id: id.to_string(),
            source_type: ReconstructionSourceType::ArchivalPhoto as i32,
            title: format!("photo {id}"),
            uri: format!("https://example.org/photos/{id}.jpg"),
            ..Default::default()
        }
    }

    fn map_source(id: &str) -> ReconstructionSource {
        ReconstructionSource {
            source_id: id.to_string(),
            source_type: ReconstructionSourceType::Map as i32,
            title: format!("sanborn {id}"),
            ..Default::default()
        }
    }

    fn spec_with_sources(mass_sources: Vec<ReconstructionSource>, facade_sources: Vec<ReconstructionSource>) -> ReconstructionSpec {
        ReconstructionSpec {
            spec_id: "recon-test-1900".to_string(),
            spec_version: 1,
            mass: Some(Mass {
                provenance: Some(PartProvenance {
                    sources: mass_sources,
                    part_confidence: 0.9,
                    ..Default::default()
                }),
                stories: 2,
                ..Default::default()
            }),
            facades: vec![Facade {
                provenance: Some(PartProvenance {
                    sources: facade_sources,
                    part_confidence: 0.8,
                    ..Default::default()
                }),
                facade_side: "front".to_string(),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn no_photos_selects_description_only() {
        let spec = spec_with_sources(vec![map_source("sanborn-1")], vec![map_source("sanborn-1")]);
        let decision = select_tier(&spec, &TierThresholds::default());
        assert_eq!(decision.tier, RenderTier::DescriptionOnly);
        assert!(decision.photo_sources.is_empty());
    }

    #[test]
    fn one_photo_selects_single_facade_tier() {
        let spec = spec_with_sources(vec![map_source("sanborn-1")], vec![photo_source("photo-1")]);
        let decision = select_tier(&spec, &TierThresholds::default());
        assert_eq!(decision.tier, RenderTier::SingleFacadePhoto);
        assert_eq!(decision.photo_sources.len(), 1);
        assert_eq!(decision.photo_sources[0].uri, "https://example.org/photos/photo-1.jpg");
    }

    #[test]
    fn duplicate_photo_across_parts_counts_once() {
        let spec = spec_with_sources(vec![photo_source("photo-1")], vec![photo_source("photo-1")]);
        let decision = select_tier(&spec, &TierThresholds::default());
        assert_eq!(decision.tier, RenderTier::SingleFacadePhoto);
        assert_eq!(decision.photo_sources.len(), 1);
    }

    #[test]
    fn few_photos_select_sparse_tier() {
        let spec = spec_with_sources(
            vec![photo_source("photo-1"), photo_source("photo-2")],
            vec![photo_source("photo-3")],
        );
        let decision = select_tier(&spec, &TierThresholds::default());
        assert_eq!(decision.tier, RenderTier::SparsePhotos);
        assert_eq!(decision.photo_sources.len(), 3);
    }

    #[test]
    fn many_photos_select_splatting_tier() {
        let mass_photos: Vec<_> = (0..8).map(|i| photo_source(&format!("photo-{i}"))).collect();
        let spec = spec_with_sources(mass_photos, vec![]);
        let decision = select_tier(&spec, &TierThresholds::default());
        assert_eq!(decision.tier, RenderTier::ManyPhotos);
    }

    #[test]
    fn metadata_records_tier_and_sources() {
        let spec = spec_with_sources(vec![], vec![photo_source("photo-9")]);
        let decision = select_tier(&spec, &TierThresholds::default());
        let metadata = decision.metadata();
        assert_eq!(metadata["renderTier"], "tier_a_single_facade_photo");
        assert_eq!(metadata["photoSourceCount"], "1");
        assert_eq!(metadata["photoSourceIds"], "photo-9");
    }
}
