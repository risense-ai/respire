# Core C ABI / JSON business contract

ABI version `0x00010002`, JSON schema `1`, opaque artifact format `1`.
The canonical header is `respire_core.h`; Core keeps an identical copy.

Core exports only `rs_core_abi_version`, `rs_core_create`, `rs_core_call`,
`rs_core_call_with_transport`, `rs_core_model_load`, `rs_core_model_load_with_host`,
`rs_core_register_providers`, `rs_core_buffer_free`, and `rs_core_destroy`. A caller owns its input; Core owns
each returned buffer until that exact buffer slot is freed once by Core. Handles
are thread confined. The safe Rust SDK supplies RAII and cannot be sent or shared.
A panic poisons the handle; destroy and recreate it. Do not share allocators.

`config` may be empty or UTF-8 `{}`; its only optional field is `test_mode` (false
by default), explicitly enabled by a fixture host. Other fields are rejected.
Inputs are UTF-8 JSON byte slices, without a trailing-NUL requirement. Output
`rs_buffer.len` is the exact byte length. Initialise output slots to zero before
each call; release success and error buffers through `rs_core_buffer_free` once.
That function clears the original slot. A null handle can be destroyed, but a
destroyed handle or a copied owned buffer cannot be reused. Pointer validity and
exclusive ownership are caller obligations, not recoverable validation errors.

| C status / JSON error code | Meaning in the current implementation |
|---|---|
| 0 | Success; JSON contains `result` |
| 1 | Invalid pointer/length, create configuration or request envelope |
| 2 | Unsupported request schema version |
| 3 | Unsupported operation |
| 4 / 5 | Reserved header values (`MODEL_UNAVAILABLE` / `ARTIFACT_INCOMPATIBLE`); not currently emitted as distinct business codes |
| 6 | Business/engine failure, including unavailable model, invalid DTO and corrupt or incompatible artifact; inspect `error.message` |
| 7 | Panic or poisoned handle; destroy and recreate |

For invalid output pointers or a null call handle, only the C status is available;
no JSON error buffer is promised. If JSON could not be parsed, or a panic happened,
`request_id` is empty. SDK callers must check both the C status and response shape.

Requests contain `schema_version`, `request_id`, `operation`, and a business payload.
Responses echo the schema/id and contain either `result` or `error` (code/message).
The request schema lists operations. DTOs are in the safe `core-sdk/src/business.rs`;
typed business adapters are in the SDK and CLI `classify.rs`.
`request.schema.json` describes the envelope and main typed DTOs;
`response.schema.json` describes the success/error envelope. Neither schema
describes feature contents. The SDK adapters remain authoritative
for operation-specific report DTOs and classify argument ordering.

| Business operation | Input / output boundary |
|---|---|
| prepare | Authorized MemoryEntry or content + model → opaque compressed index bytes |
| query | Authorized snapshots + MemoryQuery + plain/scored/contextual mode → final entries, final relevance and selected context |
| remember_candidates | Model + authorized snapshots + MemoryQuery → final merge/parent proposal lists |
| candidate_report / analyze_duplicates | Snapshots and user settings → final duplicate/parent proposals |
| tree_cure / deepen_plan / tree_float | Metadata/features + settings → final tree maintenance proposals |
| taxonomy_classify / classify_business | Authorized content/tree material and explicit provider configuration → final classification decisions or action preview |
| query_business | Authorized snapshots and explicit local/quality mode → final selected entries, warnings and routing receipt |
| memory_metadata_validate | Complete authorized entry and optional review/intent → canonical record, claim and intent digests |
| memory_evidence_evaluate | Authorized snapshots or resident view plus explicit evidence_context → current evidence, eligibility and review reasons |
| index_generation / index_status | Index generation and local index readiness |
| resident_publish / resident_release | Authorized snapshot delta and opaque view identity → immutable resident view lifetime |
| index_reusable | Previous and current authorized entries → whether derived features may be reused |
| model_status / model_probe / engine_control | Injected-model diagnostics and in-memory reset/status controls |
| backend_load_probe | Fresh host-buffer native load and inference, with synchronous host module/resource verification |

### Selected backend load probe (contract 1)

`capabilities.backend_load_probe_contract=1` advertises the new JSON operation
`backend_load_probe`. ABI `0x00010002`, schema 1, existing C exports and existing
model/status operations remain unchanged. Use `rs_core_call_with_transport` with
the ordinary request envelope. The payload has exactly `config`, `model_base64`,
`tokenizer_base64` and `text`. `config` is the usual explicit native model config,
including `model`, `asset_id`, `engine`, `execution_timeout_secs`, and optional
embedding/ONNX/provider settings. `asset_id` must be a nonempty string before any
host load callback runs. `reuse_current` must be absent or the boolean `false`;
`true`, null and other types are rejected before the host load callback. Backend
`host_transport` is unsupported; MLX requires the existing Apple Silicon feature.
Invalid/unsupported input and host failures return business error 6 with the cause,
never a successful readiness receipt. A missing transport also fails explicitly.

This diagnostic accepts base64 **memory buffers**, not paths. Unlike the ordinary
raw-buffer model-load ABI, JSON incurs base64 and decoded-buffer copies. Hosts
should bound diagnostic request sizes and use an appropriate self-contained probe
fixture for the selected backend/profile. The fixture identity is returned as
`index_identity`; qualification of the actual production model requires its actual
buffers. CPU-owned ORT mode requires the same ORT container as ordinary inference.
Model/tokenizer checksum fields, when present, are checked against decoded bytes.
Core does not open files or resolve paths, environment variables or credentials.

The synchronous transport callback receives these requests in order. All have
`operation:"backend_load_probe"`. The first three phases require `{ "ok": true }`;
a callback error or any other acknowledgement aborts the probe.

1. `phase:"load"`, `backend`, `engine`: the host loads and validates its pinned
   runtime libraries/resources before Core first touches ORT in this operation.
   This is an action request, not an already-loaded prerequisite. Existing linked
   ORT loading behavior is unchanged; the host must reject an unexpected module
   that was already loaded by the OS. Keep native library handles alive throughout
   the call and for any other sessions using them.
2. ONNX only: `phase:"register"`, `environment_address`, `api_address`: perform the
   same host provider registration used with `rs_core_register_providers`.
3. Normal ONNX only: `phase:"configure"`, `options_address`, `environment_address`,
   `api_address`, `device_address`, `host_execution_provider`, `execution_identity`:
   perform the same synchronous configuration as `rs_core_model_load_with_host`.
   A non-null device requires the host to append that device, as in the existing
   ABI. With a null device, do not append a second built-in provider; append the
   explicit custom provider only when `host_execution_provider:true`. Effective
   CPU-owned ORT mode skips this phase, matching its existing callback policy.
4. `phase:"verify"`, `observation`: runs after native session construction and one
   successful embedding, while the temporary session is still alive. The host
   resolves the supplied ONNX function addresses to their actual loaded images,
   checks provider modules and required dynamic/data resources against its pinned
   manifest, and returns the evidence below. MLX verification must inspect the
   loaded MLX/Metal images and resources; Core does not supply MLX symbol addresses.

Addresses are hexadecimal strings for pointers in the **same process**. They are
borrowed only for that callback; do not retain, dereference after return, unwind,
unload libraries in use, or reenter Core. This operation cannot be forwarded to
another process as a generic remote transport. Host wrappers must opt into these
native callbacks and keep them separate from external embedding/HTTP handling.

The verification response has these exact fields (unknown fields are rejected):

```json
{
  "verified": true,
  "resources_verified": true,
  "provider": "CPUExecutionProvider",
  "runtime_api_version": 27,
  "modules": [{
    "id": "pinned-runtime-module-id",
    "sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
    "symbols": {"CreateSessionFromArray": "0x1234", "Run": "0x5678"}
  }]
}
```

The addresses and API version above are illustrative; use the actual observation.
`runtime_api_version` must equal `observation.runtime.api_version` for ONNX. Every
observed ONNX symbol must appear at its exact address in a verified module.
ONNX observations must contain a positive API version, a non-null API address,
and both `CreateSessionFromArray` and `Run` symbols. API and symbol addresses
must use `0x` hexadecimal notation, be nonzero, and fit the current process pointer
width. A missing symbol table, unknown runtime kind, or unknown selected provider
is rejected rather than treated as an unverified custom backend. Only the exact
`Host` selection permits a host-attested custom provider name.
The nonempty module list contains opaque manifest IDs, lowercase file SHA-256
values and optional symbol maps, never filesystem paths. Include the selected
provider's modules and required loaded dependencies as well as ORT. `provider`
must match Core's built-in selection (CPU/DML/CoreML/NPU), or be `MLX CPU`/`MLX GPU`.
For the existing custom `Host` selection, the host attests the actual provider
name it configured; Core does not independently inspect custom provider identity.
MLX omits `runtime_api_version`; its module evidence comes entirely from the host.

Success returns `status:"loaded"`, `ready:true`, `fresh_session:true`,
`inference_completed:true`, `backend`, `engine`, `selected`, `dimensions`, `model`,
`index_identity`, `runtime`, `host_identity`, `module_identity_source:"host_verified"`,
`node_assignment:"not_measured"` and the existing `cpu_node_fallback` policy.
ONNX runtime observations include the actual API table address, API version,
runtime build info, and `CreateSessionFromArray`/`Run` function addresses. The fresh
session uses the same `load_memory`/`create_session`/ONNX commit or MLX load path as
inference; the effective CPU-owned mode uses the same `load_cpu_owned` path.
The probe runs only that selected backend, without a CPU comparison session.
As with inference, DML/CoreML may assign individual nodes to CPU; this does not
prove accelerator placement. The temporary session is dropped after verification,
and the shared model, handle model config and model generation are not replaced.
The resident timeout diagnostic is restored after both successful and failed
temporary loads; the probe's own timeout does not become the resident setting.
Existing queue/deadline/health controls still apply; a stalled native session can
require host recovery. Hosts must impose deadlines on their own callbacks too.

The receipt distinguishes Core's fresh load/inference observation from trusted
host evidence. Core checks its shape and symbol bindings but cannot independently
check file hashes or resource reads without violating its memory-only boundary.
Neither `capabilities`, `inference_status`, a cached heartbeat nor an `idle` state
can substitute for this operation. Keep pre-initialization package/file checks
separate from load observations so cold initialization cannot depend on itself.
SDK publication, wrapper adapters and version pins are separate integration work.

`capabilities.retrieval_contract=1` identifies query routing and ablation support.
Optional `query_settings.route_on` defaults to true; `ablate` defaults to empty
and names comma-separated semantic/kw/recency/hot/emo/tree axes. The host owns
environment parsing. Recall calibration contract 1 normalizes remaining active weights;
`normalize_ablation=false` retains historical scoring. See
[the complete calibration contract](../../docs/recall-calibration.md).
`query_business.routing` contains `temporal`, `route_on`, normalized `ablate`,
`read_heat`, `heat_source` and a nullable `heat_disabled_reason`. Scoring and
receipts share the same Core predicates. `read_heat_contract=1` accepts
`query_settings.read_heat` (default true): hot ablation wins, then configured
feedback-good scoring, then legacy recall count when enabled. Disabling read
heat leaves the hot axis weight and normalization unchanged; zero feedback
does not fall back to recall count.
The same contract also accepts optional `query_settings.now` in strict UTC
millisecond format. New hosts pass one explicit request time to ranking and
evidence. Omitted time without evidence context retains the legacy clock path
as the explicitly approved compatibility exception. This exception applies
only to old query callers; new evidence algorithms, other I/O and new request
paths do not gain clock or infrastructure access.
`query_business.retrieval` adds gate diagnostics; explicit `debug_score` includes
score components under recall calibration contract 1. Vectors and token lists
remain private. Existing query modes
and C entry points retain their response shape and ABI version.

`memory_metadata_contract=1` adds the two metadata operations and opt-in
`evidence_context` on query/query_business. Omitting that context preserves the
legacy retrieval path. Its presence filters invalid records and ancestors from
one immutable view, marks unverified clues, and adds `evidence` plus
`filtered_by_evidence` to the result. With a render budget, Core inserts evidence
labels before counting the complete output. The host must verify authority and
root evidence for each request; caller-supplied actor strings never establish
trust. See [metadata contract 1](../../docs/memory-metadata.md).

`capabilities.render_budget_contract=1` identifies optional `render_budget` on
`query` and `query_business`. Core selects complete old-format titles in the final
rank order and enforces the host's budget in Unicode scalar values, including
count-dependent prefixes, separators and the suffix. With a budget, `query`
returns `{items,budget}` and `query_business` adds `budget` to its existing result;
calls without the field are unchanged. The host must verify the final text again
before sending. See [render budget contract 1](../../docs/render-budget.md) for
the exact input, receipt, empty-result and tokenizer semantics.

`capabilities.brief_render_contract=1` identifies the separate opt-in
`render_budget.mode:"brief"` path on `query` and `query_business`. It requires
`evidence_context` and binds every supplied variant to the same authorized view,
current record revision and request time before retrieval or model use. Core
tries complete full, summary and title variants in that order, counts evidence
markers and wrappers, and returns the selected text unchanged in the budget
receipt. Omitted mode retains the title contract. SDK manifests advertise the
same capability; host completeness qualification and product activation remain
separate release gates. See [brief contract 1](../../docs/render-representations.md).

`capabilities.render_min_score_contract=1` identifies optional top-level
`query_business.render_min_score`. It accepts a finite f32, including negative
values, and retains final selected entries whose public score is at least that
value before applying the render budget. It does not change retrieval gates,
ranking or quality selection. Invalid values fail before retrieval or a model
callback. When supplied, the result includes `filtered_by_score`; omitted input
preserves the prior response shape. The same field is recorded in SDK manifests.

`capabilities.howto_recall_contract=1` identifies optional `query_business.intent`
and host-supplied `skill_runs` counts. When explicitly enabled, Core preserves the
CLI's existing detector, skill-only second pass, run-history adjustment and
procedure promotion before the final score filter and render budget. Both passes
share one authorized immutable view and each retains fast/quality fallback
behavior. The result adds `intent`, `procedure_pass` and `procedure_promoted`;
the primary pass retains its warning, routing and retrieval receipt. Omitted
intent preserves the original single-pass interface, so an old no-budget host
can retain its existing procedure branch. See the render budget contract for the
exact fields and ordering. SDK manifests advertise the same capability.

`capabilities.superseded_status_contract=1` identifies the narrow
`related_business.all_superseded=true` metadata mode. It requires empty ids/pairs
and `related_settings.enabled=false`, returning the existing RelatedResult shape
with the complete superseded map inside the authorized query scope. It reuses
the forward-edge/chain calculation and returns before association ranking; no
model or recall is involved. Omitted/false flags preserve the seed-based API and
its 256-seed, uniqueness and authorization checks. The SDK manifest has the same
capability field.

`capabilities.inference_execution` is `in_process`. The resident runtime loads
one foreground and one background native ONNX session. Handles share each resident
session, but never execute concurrent Run calls on the same session. Original model
buffers and tokenizer are shared; ORT session state and workspaces remain independent. There is no inference child or pipe protocol.
The host owns all model locations and CPU/GPU/NPU configuration. It injects raw
model/tokenizer buffers and explicit settings into each handle; matching sessions
can be shared from an in-memory cache. Account switching does not write settings.
Engine control supports `get`, `reset` and `inference_status`; `set`, `reset_cpu`
and accelerator installation/discovery report explicit host-managed errors.
Native errors retain
their underlying cause in `error.message`; no engine fallback is applied.
CPU probes reuse the shared session. Accelerator probes compare with a CPU session.
Reset invalidates existing session handles. Each inference session uses its own
FIFO queue with up to 32 waiting requests and a 120-second queue wait limit. Each native run
has a separate 120-second execution limit using ONNX cooperative cancellation.
Session loading uses a separate 120-second ONNX load cancellation deadline;
matching warm handles do not acquire a Run permit. Changing engines waits for both
sessions to release their permits. Run watchdog state is independent for each session.
Expired queue entries are removed before inference. An expired native run produces
no embedding. `inference_status` reports queued/active work, phase, capacity, limits and
`host_recovery_required` without waiting for the native session mutex. Providers
may ignore cancellation; an unresponsive native call requires host runtime recovery.
The execution limit does not guarantee termination of a native thread or release
of database locks. No additional inference process is created.

`lexical_only` requests use a vector-free metadata copy for query, remember_candidates,
and candidate_report, including mixed indexed/pending views. The copy uses the supplied
full entry body where available. Original resident vectors remain unchanged for semantic
requests; hashing query vectors are never compared with stored model vectors.

Index compatibility depends on the model and artifact generation, not the CLI
or SDK release number. Reuse complete compatible artifacts during upgrades.

The production M3 file is `onnx/model_quantized.onnx`, pinned to
`Xenova/bge-m3` revision `4de13258303883538bd53b696b452bf8099f0858`:
569694530 bytes, SHA-256
`0826f8c1ab9edf1801db86c61919d4d108e8bfc0b809ec823ad366882ff0b77d`.
The tokenizer remains at the same revision and checksum. This model has a separate
index generation from FP16 and `model_int8.onnx`; do not mix their vectors.
Retain resumable checkpoints and activate the new generation only after completion.

`Prepared.artifact` contains base64-encoded opaque compressed index bytes. Core
never reads or writes files, directories or databases. The host persists these
bytes and resolves existing `rsi1` locators into bytes before calling Core; Core
rejects unresolved locators. The compression payload, revision and generation stay
unchanged, so this boundary change alone does not require index rebuilding. Hosts
must not interpret internal index/vector structures. Index rows remain bound to
source ciphertext and generation and never enter synchronization envelopes.
Account encryption keys, API credentials, endpoints and HTTP execution remain in
the host. Core accepts only a provider name and model. For external model work,
`rs_core_call_with_transport` borrows synchronous host request/release callbacks;
Core plans the request and interprets the response. The host owns authentication,
proxy, timeout and retry execution. Callback input contains `provider`, `body`,
`timeout` and `retries`; it is transport traffic, never a returned business plan.
The host must return JSON and sanitize errors before forwarding them to Core.
Callback output is host-owned until the release callback, including failures.
Callbacks must not unwind or recursively enter the same handle. No callbacks or
credentials are retained by Core. Local mode makes no external model request.
Provider configuration is parsed before local dispatch too; local mode does not
permit credential or endpoint fields. Windows execution-provider library registration and profiling/cache paths
also belong to the host. `rs_core_register_providers` borrows `OrtEnv*` and
`OrtApi*` for synchronous registration. `rs_core_model_load_with_host` similarly
borrows `OrtSessionOptions*`, `OrtApi*`, `OrtEnv*` and an optional selected
`OrtEpDevice*` while loading. When a device is supplied, the host appends it with
`SessionOptionsAppendExecutionProvider_V2` and host-owned cache options; Core does
not append the same provider again. A null device permits profiling configuration. No paths or callbacks are retained by Core. Hosts must
not retain pointers, unwind, or reenter Core. Cache hits do not reconfigure an
existing session; include configuration changes in `session_options_id`.

`rs_core_model_load` accepts UTF-8 configuration plus separate borrowed raw model
and tokenizer buffers, never large base64 JSON or file paths. Configuration
requires `model`, host-validated `asset_id`, `engine` and
`execution_timeout_secs`; `session_options_id` is optional. The output is the
standard JSON envelope with empty request ID and `result.dimensions`. Core retains
owned bytes. With explicit `reuse_current: true`, both buffers must be empty and
the complete configuration must match a cached model. Otherwise loading fails and
the host must provide the verified buffers. Existing ABI entry points remain;
consumers must use the matching ABI 1.2 SDK for these new symbols.

SDKs advertising `inference_thread_contract: 1` accept optional
`background_intra_threads` (integer 1–16, default 1) on borrowed and streamed
model initialization. Foreground sessions remain single threaded. Core reads
this explicit configuration only; the host owns environment and resource policy.
Changing the background count changes model cache identity, including explicit
cached reuse. The same policy applies to legacy ONNX and CPU-owned ORT lanes.

For `prepare`, provide `model` and either `entry` or `content`; an `entry` takes
precedence if both are present. The default model is `m3`; custom profile IDs are supported.
`test-hash:<dimensions>` is only an explicit fixture provider and requires
`test_mode: true` in handle creation; production callers must not select it.
Successful prepare returns non-empty opaque `artifact` bytes.

`query` requires `model`, `snapshots`, `query` and `mode` (`plain`, `scored`,
`contextual`). Snapshot metadata fields are explicit strings/flags, not the sync
`StoredMemory` serialization: its local fields are deliberately skipped by serde.
An authorized entry may be `null` when only metadata is needed, and features may
be `null` when none exist. An entry ID must match its snapshot ID, and snapshot IDs
must be unique. Pass the full candidate set required for context; the final result
is an entry array, a `[final_score, entry]` array, or contextual objects containing
`score`, `entry`, and `ancestors`, respectively. Final scores are business results,
not intermediate vectors or rank-stage traces.

Do not cache artifacts independently of their source ciphertext, selected model
and index generation. Updates/deletions invalidate derived rows; a model or format
change requires rebuilding. Compatibility input is for existing records.

`remember_candidates` requires `model`, `snapshots` and `query`. Its result is an
object with `merge` and `parent` arrays, each containing `[final_score, MemoryEntry]`
pairs. SDK callers display these final proposals and authorize writes; they do
not reproduce Core grouping rules.

Final scores are user-facing results. Intermediate vectors, request plans and
execution traces are not returned by these operations.

SDK builds require Rust 1.95.0 with the exact release/commit recorded in manifest,
panic=unwind, LTO disabled, and matching target/CRT. Windows uses the dynamic CRT.
Cargo validates the pinned manifest and every file SHA-256 and stages runtime DLLs
beside executables/tests. Distributions must also include runtime libraries and
third-party notices (`stage-core-runtime.mjs`); an exe alone is incomplete on Windows.

Association contract: `related_business` accepts authorized snapshots, query,
final hit IDs and local pair evidence. It returns `related` (ID/title/source/type)
and `superseded` (hit ID to latest active replacement ID). This additive operation
keeps the existing C ABI, request envelope and opaque index generation unchanged.
See the request/response schema definitions. Capability negotiation is required
before relation writes. Public callers never receive intermediate vectors or
policy thresholds. Empty relation fields are compatible with old payload reads;
all shared-library writers must be upgraded before enabling relation writes.

Query tuning is explicit `query_settings` data (`recall_min_score`, `mmr_lambda`,
`ancestor_budget`, `ancestor_root_floor`). Association tuning is explicit
`related_settings` data (`enabled`, `max`, `pair_min`, `min_cos`, `knn_seeds`,
`knn_per`, `floor`, `ppr`). `floor` defaults to 5 and `ppr` to true; callers needing
the previous exclude-all staged order use `floor >= ids.len()` and `ppr: false`.
To bound the dense graph, requests accept at most 256 seeds, `max <= 64`,
`knn_seeds <= 16`, and `knn_per <= 16`; larger settings fail explicitly. At most
256 gathered candidates enter the walk. Core supplies existing algorithm defaults when omitted but never reads
host environment variables. Hosts translate supported environment/configuration
inputs before entering Core. No intermediate scoring traces are printed by Core.


## Experimental model input stream contract 1

This optional additive interface preserves ABI 1.2 and every borrowed entrypoint.
A statically linked host must compile references only for a pinned SDK manifest
advertising `model_input_stream_contract: 1`; runtime discovery alone cannot link
new functions against old archives. Capabilities reports 0 when CPU-owned support
is disabled. The experimental host feature is off by default. No model paths,
file handles, writable model pointers or host allocation ownership cross the ABI.

`rs_core_model_input_begin` reserves Core-owned capacity for a nonzero model up to
1 GiB, returns a nonzero globally unique ticket, and permits one Pending per handle.
The process-wide staging budget is 1 GiB of declared bytes, not a resident-memory
limit. Allocation failure releases its reservation. Tickets never wrap or recycle.
`append` borrows and copies at most 4 MiB at the exact initialized prefix; null+0
is allowed at that prefix. Bad offset, length, chunk or ticket leaves input intact.
The output's `received` field is the new prefix. Capacity never grows after begin.

All calls on a handle remain serial and require exclusive access to a live handle.
Output slots must be distinct, writable and initialized empty. Raw pointer validity
is the caller's obligation; null/length checks are not arbitrary pointer validation.
`commit` rejects null core/output slots before consumption. With a matching ticket,
it removes Pending before parsing and consumes on every later outcome, including
incomplete input, invalid config/tokenizer, unsupported mode, busy replacement,
cache hit, native failure or panic. A wrong/stale ticket cannot remove newer input.
The response uses the existing envelope and buffer-free function. CPU-owned ORT is
the only supported mode; cached reuse and provider callbacks are not accepted.

The sealed Vec is moved without a full-buffer conversion into the existing unique
owner. Both scoped native lanes borrow it; sessions stop before backing destruction.
The staging reservation survives construction and failure cleanup until publication
or actual release. `abort` releases a matching Pending and is an idempotent no-op
for stale/unknown tickets. Destroy releases Pending; it does not imply unloading a
successfully published process-global model. Existing leases/reset/retirement govern
committed lifetime. Busy replacement preserves the old group; failure after its
retirement does not promise rollback.

Cancellation before commit drops/aborts the upload. Concurrent abort or destroy
during synchronous commit is illegal. Existing native deadlines and whole-worker
recovery remain the cancellation path after acceptance; backing cannot be freed
while native users remain. PANIC poisons the handle; destroy it instead of reuse.

The host verifies the pinned model length and SHA-256 incrementally using a bounded
scratch buffer, verifies exact EOF, and closes the model file before commit. Core
hashes its accepted private prefix as each chunk is copied, checks the completed
digest against the configured digest when supplied, and uses the same actual digest
for index identity. It does not re-read the complete staged buffer to hash it.
Tokenizer bytes remain borrowed during commit; one actual tokenizer digest serves
both Core integrity comparison and index identity. Host pin verification remains
independent and the v1 ABI is unchanged. This removes overlapping host model input at
first load; it does not establish a comparable idle reduction. Replacement can
still overlap an old committed model and a new staging allocation.

This is not a streaming parser for a borrowed ONNX model. The pinned native
`CreateSessionFromArray` accepts one contiguous input; its ONNX branch parses a
protobuf model, while the direct-borrow settings apply only to ORT-format
initializers. Non-owned ONNX loads preserve the established borrowed API.
Raising the v1 staging limit or routing its completed Vec to the borrowed loader
would not bound the native parse and session memory. A future general contract
needs an authenticated backing/initializer design and a separate memory and
cancellation acceptance gate, not merely a higher input-size limit.

`capabilities.feedback_contract=1` and the matching SDK manifest field identify
optional `query_settings.feedback`: `{enabled:false, good:{}, bad:{}, bad_penalty:true}`.
Counts are nonnegative u64 integers keyed by complete authorized memory ID;
unknown feedback fields and non-boolean switches are rejected. Missing/null or
explicitly disabled feedback preserves the legacy read-frequency ranking path.
When enabled, good counts replace the existing heat input and bad counts apply
the scientist baseline penalty before selection. Hosts own query-conditioned
aggregation, provenance, 180-day filtering, authorization and per-request refresh.
No mutable global counters, environment or store access is introduced in Core.
The canonical JSON schema describes the same additive DTO; SDK capability
metadata identifies support. SDK archives retain their existing lib/header/runtime/notices layout.

## Embedding profiles (contract 1)

SDK capabilities and manifests advertise `embedding_profile_contract: 1` and
`embedding_transport_contract: 1`. Existing C symbols and the old M3 configuration
remain supported. `model` is a host-chosen profile ID, not a closed enumeration.
`backend` is `onnx` (default) or `host_transport`. The host owns profile storage,
paths, downloads, verified bytes, provider URLs, credentials and request execution.

The optional `embedding` object has required `model_id`, `revision`, `dimensions`
(1–65536), `max_tokens` (4–131072), `pooling` (`cls`, `mean`, or
`sentence_embedding`) and `normalize`. Optional `query_prefix`, `document_prefix`
default to empty strings; `pad_token_id` defaults to 0 and `pad_to_max_tokens`
defaults to false. Omitting the object is supported only for legacy `m3`.
M3 uses 1024 dimensions, a 512-token window, CLS and normalization. A host preset
for bge-small-zh-v1.5 uses 512 dimensions, a 512-token window, CLS, normalization
and the model-specific search query prefix defined by the host preset.
Both presets are ordinary editable configurations. Core does not select resources.

The optional `onnx` object supplies `input_ids` (required when the object is
present), optional/null `attention_mask`, `token_type_ids`, and `output` names.
Omitting the object uses conventional input names and the first output. Inputs
are bound by name as int64; a missing optional input is omitted. Output is float32
with shape `[1, tokens, dimensions]` for CLS/mean or `[1, dimensions]` for
sentence embeddings. Mean pooling excludes masked padding. Prefixes and special
tokens count against the token window. NPU execution pads to that window using
the configured pad ID. Existing cancellation, deadlines and resource leases apply.
The CPU-owned ORT extension retains the legacy M3 settings; other profiles use
the general ONNX path and borrowed-buffer loading.

For a custom native execution provider, the host sets `host_execution_provider:
true` and a nonempty `execution_identity`. Core skips built-in provider selection
and invokes the existing native-options callback with a null device. The host
selects and configures the provider. `execution_identity` binds changes that may
alter embeddings; `session_options_id` separately covers execution-only settings.
CPU reference probes clear the override and use the standard CPU provider.

`index_generation` accepts `{"model":"profile-id","config":{...}}` before
loading. ONNX configuration supplies lowercase `model_sha256` and
`tokenizer_sha256`; loading checks the optional checksum pair against actual bytes.
Core binds opaque artifacts to the actual hashes, dimensions, document prefix,
pooling, normalization, token window, padding and tensor names. Query prefixes,
thread counts and execution timeouts do not change document identity. Query
configuration still participates in model cache reuse. The known quantized M3
and CPU ORT weights with the original tokenizer and document settings preserve
the legacy M3 identity. Incompatible artifacts fail explicitly; the host stages
re-embedding before activating a different generation. Resident views are bound
to the same identity, and semantic retrieval validates all vector dimensions.

`host_transport` configuration requires `provider` and an explicit `embedding`
object. Optional `protocol` is `openai_embeddings`. Load with empty model and
tokenizer buffers. Every business call borrows its own callback through
`rs_core_call_with_transport`; Core never retains callbacks or makes HTTP calls.
The callback receives `{"operation":"embedding","protocol":"openai_embeddings",
"provider":"name","model":"model-id","input":["prefixed text"],
"timeout":120,"retries":0}` (`timeout` uses `execution_timeout_secs`). Return an OpenAI embeddings response containing
exactly one `data` item with `index: 0` and an `embedding` array. Core validates
its dimension, finite values and nonzero norm, then normalizes when configured.
Only opaque artifacts or final business results leave the business API. Transport
errors propagate, and callback output uses the existing release callback on all
paths. External probes report availability and dimensions without claiming a CPU
reference or cosine comparison. External revision identity is host-declared;
hosts must update it when a provider changes the effective model.

For `host_transport`, `max_tokens` only declares the service's input capacity.
Core has no tokenizer for this backend and does not count tokens, truncate input
or enforce an exact token window. Character-based retrieval chunks do not
guarantee that the token budget is met. The provider must explicitly reject input
that exceeds its capacity, including prefixes and special tokens. Services that
silently truncate input are unsupported. Core does not add `max_tokens` to the
OpenAI embeddings request body.

## Version 1 topology implementation and wire boundary

The existing `deepen_plan` operation uses a request-local parent-to-child
index without exposing a new operation, capability or payload. Valid inputs,
thresholds, ranking inputs, clustering and response shapes are unchanged.
The topology and counting stage is linear in the catalog; pairwise vector
clustering still depends on the square of the number of embedded children.

Two malformed legacy inputs are deliberate safety exceptions: slicing a
non-ASCII root prefix at a non-UTF-8 boundary previously could panic and poison
the handle; a reachable cyclic parent graph previously did not terminate.
Both now return the ordinary engine failure envelope without poisoning the
handle. Unknown or ambiguous valid prefixes and non-flat-root diagnostics keep
their existing code, request ID, and message. Keep these safety cases distinct
from the valid-input equivalence claim.

This repository's schema version, not a Rust package or product version, is the
wire contract. Core decodes authorized `Snapshot` fields and opaque features
into a private non-serializable candidate. Host storage rows, ciphertext and
storage adapters are not shared wire types. Version 1 fixtures in
`fixtures/wire-v1-compat.json` show older plaintext entry defaults and flattened
entry-field preservation, tolerant optional query fields, and the intentionally
strict snapshot envelope. New fields must respect these existing policies; hosts
cannot assume all unknown fields survive a round trip.


The v1 request envelope and snapshot DTO continue to reject unknown fields.
Use only expressly negotiated extensions on old SDKs. The GNU SDK release
check runs a fixed, reviewed CLI source commit as a compatibility fixture; it
does not wait for a newer CLI release, and product releases do not share a
version number.

Run the exported-ABI golden cases and deterministic 10k/20k topology work
comparison with `cargo test --workspace --locked deepen_plan`. The optional
wall-clock topology-only comparison is
`cargo test --workspace --locked deepen_plan_topology_relative_benchmark -- --ignored --nocapture`.
Its timing is environment-specific; the normal work proof counts exactly the
indexed traversal and legacy scan operations and does not measure clustering.
