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
    'PartProvenance',
    'ReconstructionSource',
    'Mass',
    'Facade',
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
    /message\s+PartProvenance\s+\{[\s\S]*repeated\s+ReconstructionSource\s+sources\s*=\s*1;[\s\S]*double\s+confidence\s*=\s*2;[\s\S]*bool\s+from_gnn_prior\s*=\s*3;/,
    'PartProvenance source, confidence, and from_gnn_prior fields',
);
assertContains(
    reconstructionProto,
    /message\s+ReconstructionSpec\s+\{[\s\S]*TenantContext\s+tenant_context\s*=\s*1;[\s\S]*ReconstructionSpecStatus\s+status\s*=\s*8;[\s\S]*uint32\s+version\s*=\s*9;/,
    'ReconstructionSpec tenant context and status/version fields',
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
