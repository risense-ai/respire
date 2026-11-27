# Synthetic 1.x SQLite Golden Fixture

`legacy-1x.db.b64` stores the SQLite fixture as base64 wrapped at 76 columns.
The decoded database freezes the supported unversioned TEXT-ID schema, a historical
encrypted memory, an encrypted pending sync operation, a chunk, metadata and a
legacy trigger. All credentials and content are synthetic. This is not an
export from a published 1.x installation and does not replace that release gate.

Decoded SHA-256: `1eb18c5943d16b54fb245a6372e0bc1d40d811e84d0de617afb75b8168e81716`.

The historical memory ciphertext comes from
`crates/app/src/fixtures/legacy-memory-v1/legacy_stored_memory.json` and uses
the synthetic URK `[42; 32]`. To generate the fixture, run
`node apps/runtime/scripts/generate-card014-golden-db.mjs` from the repo root.
The generator refuses to overwrite the existing fixture.
