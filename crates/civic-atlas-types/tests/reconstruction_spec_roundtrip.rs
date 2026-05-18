use civic_atlas_types::civic_atlas::v1::{
    Facade, Mass, PartProvenance, ReconstructionSource, ReconstructionSourceType,
    ReconstructionSpec, ReconstructionSpecStatus, TenantContext,
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
        confidence: 0.82,
        from_gnn_prior: true,
        reviewer_note: "Opening rhythm inferred from a checked source.".to_string(),
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
        version: 1,
        supersedes_spec_id: String::new(),
        created_at_ms: Some(1_609_459_200_000),
        updated_at_ms: Some(1_609_459_200_000),
        created_by: "reviewer-001".to_string(),
        reviewed_by: String::new(),
        mass: Some(Mass {
            provenance: Some(provenance.clone()),
            form: "rectangular-commercial".to_string(),
            story_count: 2,
            height: None,
            width: None,
            depth: None,
            attributes: Default::default(),
        }),
        facades: vec![Facade {
            provenance: Some(provenance.clone()),
            orientation: "south".to_string(),
            material: "brick".to_string(),
            color: "red".to_string(),
            opening_grids: vec![],
            attributes: Default::default(),
        }],
        roof: None,
        ornaments: vec![],
        ground_floor: None,
        assets: vec![],
        metadata: Default::default(),
    };

    let bytes = spec.encode_to_vec();
    let decoded = ReconstructionSpec::decode(bytes.as_slice()).expect("decode spec");
    let decoded_mass_provenance = decoded
        .mass
        .and_then(|mass| mass.provenance)
        .expect("mass provenance");

    assert_eq!(decoded_mass_provenance.confidence, 0.82);
    assert!(decoded_mass_provenance.from_gnn_prior);
    assert_eq!(decoded_mass_provenance.sources.len(), 1);
    assert_eq!(
        decoded_mass_provenance.sources[0].source_id,
        "source-sanborn-1921"
    );
    assert_eq!(
        decoded_mass_provenance.sources[0].source_type,
        ReconstructionSourceType::Map as i32
    );
}
