These are synthetic, frozen specimens of the former Core test-only envelope.
`legacy_stored_memory.json` holds the outer StoredMemory record, with 12-byte
hex nonces followed by AES-256-GCM ciphertext/tag in the historical fields.
The ciphertext payload is the exact UTF-8 `legacy_payload_v2.json` (including its
final newline); the encrypted embedding is `legacy_embedding.f32le` (three
little-endian float32 values: 0.25, -0.5, 1.0). The source layout is the previous
`legacy_engine::MemoryEngine::seal` implementation. The specimens use synthetic
32-byte URK `[42; 32]`, HKDF-SHA256 without salt with labels
`onememory:data:v1` and `onememory:embedding:v1`, and fixed nonces
`000102030405060708090a0b` and `0c0d0e0f1011121314151617` respectively.
They contain no account or production data. Core preserves only the static
specimens, not the historical schema as a public type or decoder. Decrypt,
SQLite, and migration belong to the host.
