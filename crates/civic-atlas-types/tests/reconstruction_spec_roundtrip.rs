use civic_atlas_types::civic_atlas::v1::{
    Facade, Mass, OpeningGrid, OpeningOverride, PartProvenance, ProvenanceCorrection,
    ReconstructionSource, ReconstructionSourceType, ReconstructionSpec, ReconstructionSpecStatus,
    TenantContext, TextureProvenance,
};
use prost::Message;

#[test]
fn reconstruction_spec_round_trips_part_provenance() {
    let source = ReconstructionSource {
        source_id: "source-sanborn-1921".to_string(),
        source_type: ReconstructionSourceType::Map as i32,
        title: "Sanborn map plate".to_string(),
        uri: "https://example.test/sanborn".to_string(),
        captured_at_ms: Some(1_609_459_200_000),
        citation: "Sanborn fire insurance map".to_string(),
        metadata: [("plate".to_string(), "12".to_string())].into(),
    };

    let provenance = PartProvenance {
        sources: vec![source],
        part_confidence: 0.82,
        from_gnn_prior: true,
        moderator_notes: "Opening rhythm inferred from a checked source.".to_string(),
        coverage_quality: 0.76,
        gnn_version: "civic-pairformer/test".to_string(),
        per_source_confidences: vec![0.82],
        moderator_overridden: true,
        moderator_overridden_at_ms: Some(1_609_459_300_000),
        has_source_conflict: true,
        correction: Some(ProvenanceCorrection {
            correction_id: "correction-001".to_string(),
            correction_type: "opening_pattern".to_string(),
            correction_reasoning: "Moderator resolved Sanborn/photo mismatch.".to_string(),
            correction_approved_at_ms: Some(1_609_459_400_000),
        }),
    };

    let spec = ReconstructionSpec {
        tenant_context: Some(TenantContext {
            tenant_id: "flint".to_string(),
            atlas_node_id: "carriage-town".to_string(),
            metadata: Default::default(),
        }),
        spec_id: "spec-building-001".to_string(),
        civic_object_id: "object-building-001".to_string(),
        building_id: "building-001".to_string(),
        parcel_id: "parcel-001".to_string(),
        block_id: "block-001".to_string(),
        title: "Carriage Town storefront reconstruction".to_string(),
        status: ReconstructionSpecStatus::Draft as i32,
        spec_version: 1,
        supersedes_spec_id: String::new(),
        created_at_ms: Some(1_609_459_200_000),
        updated_at_ms: Some(1_609_459_200_000),
        created_by: "reviewer-001".to_string(),
        reviewed_by: String::new(),
        mass: Some(Mass {
            provenance: Some(provenance.clone()),
            form: "rectangular-commercial".to_string(),
            stories: 2,
            part_id: "mass-main".to_string(),
            footprint_geometry_id: "footprint-001".to_string(),
            height: None,
            width: None,
            depth: None,
            attributes: Default::default(),
        }),
        facades: vec![Facade {
            provenance: Some(provenance.clone()),
            facade_side: "south".to_string(),
            primary_material: "brick".to_string(),
            color: "red".to_string(),
            opening_grids: vec![OpeningGrid {
                provenance: Some(provenance.clone()),
                bay_count: 5,
                floor_count: 2,
                window_pattern: "six_over_six".to_string(),
                attributes: Default::default(),
                opening_overrides: vec![OpeningOverride {
                    bay_index: 2,
                    override_kind: "window".to_string(),
                    override_pattern: "casement".to_string(),
                    override_provenance: Some(provenance.clone()),
                }],
                part_id: "opening-grid-primary".to_string(),
                has_storefront_ground: true,
            }],
            attributes: Default::default(),
            part_id: "facade-south".to_string(),
            texture_provenance: Some(TextureProvenance {
                texture_source: "archival_photo".to_string(),
                lora_archetype: "commercial_brick".to_string(),
                lora_weight: Some(0.62),
                controlnet_conditioning_source: "source-sanborn-1921".to_string(),
                texture_confidence: Some(0.71),
            }),
        }],
        roof: None,
        ornaments: vec![],
        ground_floor: None,
        assets: vec![],
        metadata: Default::default(),
        t_start_ms: Some(1_609_459_200_000),
        t_end_ms: Some(1_640_995_200_000),
        archetype_classification: "commercial-brick".to_string(),
        gnn_version: "civic-pairformer/test".to_string(),
        published_at_ms: Some(1_609_459_500_000),
        license: "CC-BY-4.0".to_string(),
    };

    let bytes = spec.encode_to_vec();
    let decoded = ReconstructionSpec::decode(bytes.as_slice()).expect("decode spec");
    let decoded_mass_provenance = decoded
        .mass
        .and_then(|mass| mass.provenance)
        .expect("mass provenance");

    assert_eq!(decoded_mass_provenance.part_confidence, 0.82);
    assert!(decoded_mass_provenance.from_gnn_prior);
    assert_eq!(decoded_mass_provenance.coverage_quality, 0.76);
    assert_eq!(decoded_mass_provenance.gnn_version, "civic-pairformer/test");
    assert_eq!(decoded_mass_provenance.per_source_confidences, vec![0.82]);
    assert!(decoded_mass_provenance.moderator_overridden);
    assert_eq!(
        decoded_mass_provenance
            .correction
            .expect("correction metadata")
            .correction_type,
        "opening_pattern"
    );
    assert_eq!(decoded_mass_provenance.sources.len(), 1);
    assert_eq!(
        decoded_mass_provenance.sources[0].source_id,
        "source-sanborn-1921"
    );
    assert_eq!(
        decoded_mass_provenance.sources[0].source_type,
        ReconstructionSourceType::Map as i32
    );

    let decoded_override = decoded.facades[0].opening_grids[0].opening_overrides[0].clone();
    assert_eq!(decoded_override.bay_index, 2);
    assert_eq!(decoded_override.override_kind, "window");
    assert_eq!(decoded_override.override_pattern, "casement");
    assert_eq!(
        decoded_override
            .override_provenance
            .expect("override provenance")
            .gnn_version,
        "civic-pairformer/test"
    );
}
