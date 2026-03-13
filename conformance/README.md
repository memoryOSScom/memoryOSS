# Runtime Conformance Kit

This directory is the public compatibility target for the portable memory runtime.

Published artifact lines:

- `memoryoss.runtime.v1alpha1`
- `memoryoss.passport.v1alpha1`
- `memoryoss.history.v1alpha1`

Layout:

- `schemas/`: JSON Schemas for the published artifact shapes
- `fixtures/`: canonical test vectors used by the compatibility harness

Reference readers/writers:

- Rust: `memoryoss conformance normalize --kind <runtime_contract|passport|history> --input <file> --output <file>`
- Python: `python3 tests/reference_conformance.py --kind <runtime_contract|passport|history> --input <file> --output <file>`
- TypeScript: `node sdk/typescript/dist/conformance.js --kind <runtime_contract|passport|history> --input <file> --output <file>`

Compatibility harness:

- `python3 tests/run_conformance_kit.py`

Versioning and support policy:

- Each artifact line is immutable once published. Breaking wire changes require a new `contract_id` or `bundle_version`.
- Additive fields are allowed within a published line. Readers must ignore unknown additive fields.
- The compatibility harness is authoritative for the currently published line set in this directory.
- Once a successor line is published, memoryOSS keeps reader compatibility for the previous published line for at least two minor releases.
- Writers default to the latest published line unless an explicit format selector is introduced later.
- Deprecation is announced in the conformance kit and public docs before a line leaves the compatibility window.
