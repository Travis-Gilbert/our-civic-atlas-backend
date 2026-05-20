from pathlib import Path
import re


REPO_ROOT = Path(__file__).resolve().parents[1]
RECONSTRUCTION_PROTO = REPO_ROOT / "proto/civic_atlas/v1/reconstruction.proto"
SERVICE_PROTO = REPO_ROOT / "proto/civic_atlas/v1/reconstruction_service.proto"

REQUIRED_PART_MESSAGES = (
    "Mass",
    "Facade",
    "OpeningGrid",
    "Roof",
    "Ornament",
    "GroundFloor",
)
REQUIRED_SERVICE_METHODS = (
    "GetReconstructionSpec",
    "ListReconstructionSpecs",
    "SaveDraftSpec",
    "SubmitSpecForReview",
    "ApproveSpec",
    "ListAssetsForSpec",
)


def require(source: str, pattern: str, label: str) -> None:
    if re.search(pattern, source, flags=re.DOTALL):
        return

    raise AssertionError(f"Missing {label}")


def main() -> None:
    reconstruction_proto = RECONSTRUCTION_PROTO.read_text()
    service_proto = SERVICE_PROTO.read_text()

    for message_name in REQUIRED_PART_MESSAGES:
        require(
            reconstruction_proto,
            rf"message\s+{message_name}\s+\{{.*?PartProvenance\s+provenance\s*=\s*1;",
            f"{message_name}.provenance",
        )

    require(
        reconstruction_proto,
        r"message\s+ProvenanceCorrection\s+\{.*?string\s+correction_id\s*=\s*1;.*?string\s+correction_type\s*=\s*2;.*?string\s+correction_reasoning\s*=\s*3;.*?optional\s+int64\s+correction_approved_at_ms\s*=\s*4;",
        "ProvenanceCorrection correction metadata fields",
    )
    require(
        reconstruction_proto,
        r"message\s+TextureProvenance\s+\{.*?string\s+texture_source\s*=\s*1;.*?string\s+lora_archetype\s*=\s*2;.*?optional\s+double\s+lora_weight\s*=\s*3;.*?string\s+controlnet_conditioning_source\s*=\s*4;.*?optional\s+double\s+texture_confidence\s*=\s*5;",
        "TextureProvenance texture metadata fields",
    )
    require(
        reconstruction_proto,
        r"message\s+PartProvenance\s+\{.*?repeated\s+ReconstructionSource\s+sources\s*=\s*1;.*?double\s+part_confidence\s*=\s*2;.*?bool\s+from_gnn_prior\s*=\s*3;.*?string\s+moderator_notes\s*=\s*4;.*?repeated\s+double\s+per_source_confidences\s*=\s*7;.*?ProvenanceCorrection\s+correction\s*=\s*11;",
        "PartProvenance USD-aligned provenance fields",
    )
    require(
        reconstruction_proto,
        r"message\s+ReconstructionSpec\s+\{.*?TenantContext\s+tenant_context\s*=\s*1;.*?string\s+spec_id\s*=\s*2;.*?ReconstructionSpecStatus\s+status\s*=\s*8;.*?uint32\s+spec_version\s*=\s*9;.*?optional\s+int64\s+t_start_ms\s*=\s*22;.*?optional\s+int64\s+t_end_ms\s*=\s*23;.*?string\s+archetype_classification\s*=\s*24;.*?string\s+gnn_version\s*=\s*25;.*?optional\s+int64\s+published_at_ms\s*=\s*26;.*?string\s+license\s*=\s*27;",
        "ReconstructionSpec identifiers, version, and USD publication fields",
    )
    require(
        reconstruction_proto,
        r"message\s+OpeningOverride\s+\{.*?uint32\s+bay_index\s*=\s*1;.*?string\s+override_kind\s*=\s*2;.*?string\s+override_pattern\s*=\s*3;.*?PartProvenance\s+override_provenance\s*=\s*4;",
        "OpeningOverride bay, kind, pattern, and provenance fields",
    )
    require(
        reconstruction_proto,
        r"message\s+OpeningGrid\s+\{.*?string\s+window_pattern\s*=\s*4;.*?reserved\s+5;.*?repeated\s+OpeningOverride\s+opening_overrides\s*=\s*7;.*?string\s+part_id\s*=\s*8;.*?bool\s+has_storefront_ground\s*=\s*9;",
        "OpeningGrid USD-aligned window pattern and override fields",
    )
    require(
        reconstruction_proto,
        r"message\s+Mass\s+\{.*?uint32\s+stories\s*=\s*3;.*?string\s+part_id\s*=\s*8;.*?string\s+footprint_geometry_id\s*=\s*9;",
        "Mass USD-aligned stories and part identity fields",
    )
    require(
        reconstruction_proto,
        r"message\s+Facade\s+\{.*?string\s+facade_side\s*=\s*2;.*?string\s+primary_material\s*=\s*3;.*?string\s+part_id\s*=\s*7;.*?TextureProvenance\s+texture_provenance\s*=\s*8;",
        "Facade USD-aligned fields",
    )
    require(
        reconstruction_proto,
        r"message\s+Roof\s+\{.*?string\s+roof_type\s*=\s*2;.*?string\s+roof_material\s*=\s*3;.*?TextureProvenance\s+texture_provenance\s*=\s*6;",
        "Roof USD-aligned fields",
    )
    require(
        reconstruction_proto,
        r"message\s+Ornament\s+\{.*?string\s+ornament_kind\s*=\s*3;.*?string\s+ornament_material\s*=\s*5;.*?string\s+ornament_style\s*=\s*7;.*?TextureProvenance\s+texture_provenance\s*=\s*8;",
        "Ornament USD-aligned fields",
    )
    require(
        reconstruction_proto,
        r"message\s+GroundFloor\s+\{.*?bool\s+has_canopy\s*=\s*5;.*?string\s+part_id\s*=\s*7;.*?TextureProvenance\s+texture_provenance\s*=\s*8;",
        "GroundFloor USD-aligned fields",
    )
    require(
        reconstruction_proto,
        r"message\s+ReconstructionSpec\s+\{(?!.*building_confidence)",
        "absence of building-level confidence storage",
    )
    require(
        service_proto,
        r'import\s+"civic_atlas/v1/civic_atlas\.proto";',
        "civic_atlas import",
    )
    require(
        service_proto,
        r'import\s+"civic_atlas/v1/reconstruction\.proto";',
        "reconstruction import",
    )

    for method_name in REQUIRED_SERVICE_METHODS:
        require(
            service_proto,
            rf"rpc\s+{method_name}\s*\(",
            f"ReconstructionService method {method_name}",
        )

    print("Reconstruction proto Python gate passed.")


if __name__ == "__main__":
    main()
