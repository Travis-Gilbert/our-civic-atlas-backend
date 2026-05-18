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
        r"message\s+PartProvenance\s+\{.*?repeated\s+ReconstructionSource\s+sources\s*=\s*1;.*?double\s+confidence\s*=\s*2;.*?bool\s+from_gnn_prior\s*=\s*3;",
        "PartProvenance source, confidence, and from_gnn_prior fields",
    )
    require(
        reconstruction_proto,
        r"message\s+ReconstructionSpec\s+\{.*?TenantContext\s+tenant_context\s*=\s*1;.*?string\s+spec_id\s*=\s*2;.*?ReconstructionSpecStatus\s+status\s*=\s*8;.*?uint32\s+version\s*=\s*9;",
        "ReconstructionSpec identifiers, tenant context, and status/version fields",
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
