import { readFile } from 'node:fs/promises';
import { resolve } from 'node:path';

const REPO_ROOT = resolve(import.meta.dirname, '../../..');
const RECONSTRUCTION_PROTO = resolve(
    REPO_ROOT,
    'proto/civic_atlas/v1/reconstruction.proto',
);
const SERVICE_PROTO = resolve(
    REPO_ROOT,
    'proto/civic_atlas/v1/reconstruction_service.proto',
);

const REQUIRED_MESSAGES = Object.freeze([
    'ReconstructionSpec',
    'ProvenanceCorrection',
    'PartProvenance',
    'ReconstructionSource',
    'TextureProvenance',
    'Mass',
    'Facade',
    'OpeningOverride',
    'OpeningGrid',
    'Roof',
    'Ornament',
    'GroundFloor',
]);

const REQUIRED_SERVICE_METHODS = Object.freeze([
    'GetReconstructionSpec',
    'ListReconstructionSpecs',
    'SaveDraftSpec',
    'SubmitSpecForReview',
    'ApproveSpec',
    'ListAssetsForSpec',
]);

function assertContains(source, pattern, label) {
    if (pattern.test(source)) {
        return;
    }

    throw new Error(`Missing ${label}`);
}

const [reconstructionProto, serviceProto] = await Promise.all([
    readFile(RECONSTRUCTION_PROTO, 'utf8'),
    readFile(SERVICE_PROTO, 'utf8'),
]);

for (const messageName of REQUIRED_MESSAGES) {
    assertContains(
        reconstructionProto,
        new RegExp(`message\\s+${messageName}\\s+\\{`),
        `message ${messageName}`,
    );
}

assertContains(
    reconstructionProto,
    /message\s+ProvenanceCorrection\s+\{[\s\S]*string\s+correction_id\s*=\s*1;[\s\S]*string\s+correction_type\s*=\s*2;[\s\S]*string\s+correction_reasoning\s*=\s*3;[\s\S]*optional\s+int64\s+correction_approved_at_ms\s*=\s*4;/,
    'ProvenanceCorrection correction metadata fields',
);
assertContains(
    reconstructionProto,
    /message\s+TextureProvenance\s+\{[\s\S]*string\s+texture_source\s*=\s*1;[\s\S]*string\s+lora_archetype\s*=\s*2;[\s\S]*optional\s+double\s+lora_weight\s*=\s*3;[\s\S]*string\s+controlnet_conditioning_source\s*=\s*4;[\s\S]*optional\s+double\s+texture_confidence\s*=\s*5;/,
    'TextureProvenance texture metadata fields',
);
assertContains(
    reconstructionProto,
    /message\s+PartProvenance\s+\{[\s\S]*repeated\s+ReconstructionSource\s+sources\s*=\s*1;[\s\S]*double\s+part_confidence\s*=\s*2;[\s\S]*bool\s+from_gnn_prior\s*=\s*3;[\s\S]*string\s+moderator_notes\s*=\s*4;[\s\S]*repeated\s+double\s+per_source_confidences\s*=\s*7;[\s\S]*ProvenanceCorrection\s+correction\s*=\s*11;/,
    'PartProvenance USD-aligned provenance fields',
);
assertContains(
    reconstructionProto,
    /message\s+ReconstructionSpec\s+\{[\s\S]*TenantContext\s+tenant_context\s*=\s*1;[\s\S]*ReconstructionSpecStatus\s+status\s*=\s*8;[\s\S]*uint32\s+spec_version\s*=\s*9;[\s\S]*optional\s+int64\s+t_start_ms\s*=\s*22;[\s\S]*optional\s+int64\s+t_end_ms\s*=\s*23;[\s\S]*string\s+archetype_classification\s*=\s*24;[\s\S]*string\s+gnn_version\s*=\s*25;[\s\S]*optional\s+int64\s+published_at_ms\s*=\s*26;[\s\S]*string\s+license\s*=\s*27;/,
    'ReconstructionSpec tenant context, version, and USD publication fields',
);
assertContains(
    reconstructionProto,
    /message\s+OpeningOverride\s+\{[\s\S]*uint32\s+bay_index\s*=\s*1;[\s\S]*string\s+override_kind\s*=\s*2;[\s\S]*string\s+override_pattern\s*=\s*3;[\s\S]*PartProvenance\s+override_provenance\s*=\s*4;/,
    'OpeningOverride bay, kind, pattern, and provenance fields',
);
assertContains(
    reconstructionProto,
    /message\s+OpeningGrid\s+\{[\s\S]*string\s+window_pattern\s*=\s*4;[\s\S]*reserved\s+5;[\s\S]*repeated\s+OpeningOverride\s+opening_overrides\s*=\s*7;[\s\S]*string\s+part_id\s*=\s*8;[\s\S]*bool\s+has_storefront_ground\s*=\s*9;/,
    'OpeningGrid USD-aligned window pattern and override fields',
);
assertContains(
    reconstructionProto,
    /message\s+Mass\s+\{[\s\S]*uint32\s+stories\s*=\s*3;[\s\S]*string\s+part_id\s*=\s*8;[\s\S]*string\s+footprint_geometry_id\s*=\s*9;/,
    'Mass USD-aligned stories and part identity fields',
);
assertContains(
    reconstructionProto,
    /message\s+Facade\s+\{[\s\S]*string\s+facade_side\s*=\s*2;[\s\S]*string\s+primary_material\s*=\s*3;[\s\S]*string\s+part_id\s*=\s*7;[\s\S]*TextureProvenance\s+texture_provenance\s*=\s*8;/,
    'Facade USD-aligned fields',
);
assertContains(
    reconstructionProto,
    /message\s+Roof\s+\{[\s\S]*string\s+roof_type\s*=\s*2;[\s\S]*string\s+roof_material\s*=\s*3;[\s\S]*TextureProvenance\s+texture_provenance\s*=\s*6;/,
    'Roof USD-aligned fields',
);
assertContains(
    reconstructionProto,
    /message\s+Ornament\s+\{[\s\S]*string\s+ornament_kind\s*=\s*3;[\s\S]*string\s+ornament_material\s*=\s*5;[\s\S]*string\s+ornament_style\s*=\s*7;[\s\S]*TextureProvenance\s+texture_provenance\s*=\s*8;/,
    'Ornament USD-aligned fields',
);
assertContains(
    reconstructionProto,
    /message\s+GroundFloor\s+\{[\s\S]*bool\s+has_canopy\s*=\s*5;[\s\S]*string\s+part_id\s*=\s*7;[\s\S]*TextureProvenance\s+texture_provenance\s*=\s*8;/,
    'GroundFloor USD-aligned fields',
);
assertContains(
    serviceProto,
    /import\s+"civic_atlas\/v1\/reconstruction\.proto";/,
    'reconstruction service import',
);

for (const methodName of REQUIRED_SERVICE_METHODS) {
    assertContains(
        serviceProto,
        new RegExp(`rpc\\s+${methodName}\\s*\\(`),
        `ReconstructionService method ${methodName}`,
    );
}

console.log('Reconstruction proto TypeScript gate passed.');
