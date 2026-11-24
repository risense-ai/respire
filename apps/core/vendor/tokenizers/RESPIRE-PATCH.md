# Respire tokenizer patch

Upstream: Hugging Face tokenizers 0.23.2, Apache-2.0; LICENSE and original notices retained. The crate is vendored so cross-platform SDK builds use the reviewed implementation without a local machine path or unpinned fork.

Runtime delta is confined to Unigram model.rs / trie.rs: an iterative checked builder seals sorted flat Node/Edge arrays; prefix iterator output ownership and model serialization are unchanged. The immutable graph's Clone increments an Arc count, never copies vocabulary/model bytes. This exception is disclosed; upstream Unigram Clone still copies vocabulary and token lookup, and the production BGE shared assets do not invoke it. Cache-hit encode moves the already-owned cached result instead of copying Vec<String> again. Existing cache.get still makes its explicit bounded copy.

The optional memory-layout-diagnostics feature exposes numerical layout and deterministic comparison data only and is disabled in production. Its helper additions to cache.rs and model.rs do not alter disabled-feature inference. No prefix-length/hybrid experiment is present. No newly added unsafe, unwrap() or expect().

Compatibility evidence: all 250002 vocabulary IDs and byte prefixes; 548 multilingual/pair/overflow/special/fuzz cases across cold/warm/serialized reload; 461 concurrent inputs; 16 small custom model outputs, all equal to controls. Three standalone trie bounds/order/100000-byte teardown tests passed. A tiny upstream suffix-training fixture exposed an esaxx panic (cause not yet verified); evidence is retained separately. The accepted BGE inference model is not trained or changed by this patch.
