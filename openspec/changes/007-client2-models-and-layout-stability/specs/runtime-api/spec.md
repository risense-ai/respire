## ADDED Requirements

### Requirement: Curated Four-Model Catalog and Full ONNX Variant Metadata

The Runtime SHALL offer only `granite-311m-r2`, `granite-97m-r2`, `m3`, and `bge-base-zh-v1.5` for selection. It MUST advertise the full ONNX inventory with dimensions, context limits, variant IDs, paths, known byte sizes, companion files, and recommendation, verification, installation, and active state. It MUST recommend `granite-311m-r2` with `quint8_avx2` as the initial default.

#### Scenario: Inspect a fresh catalog

- **GIVEN** a fresh Runtime catalog
- **WHEN** a client requests `GET /v1/models/catalog`
- **THEN** the response lists four families with variant counts of two, two, eight, and eight, and recommends `granite-311m-r2` with `quint8_avx2`

### Requirement: Variant Selection and Download Mirror

The Runtime MUST accept a variant on `POST /v1/models/downloads` and `POST /v1/models/switch`. Omitted variants use the catalog default. Downloads MUST accept mirror presets and a custom URL. `PATCH /v1/host/config` MUST persist `model_mirror`. Enabling an installed candidate MUST build a shadow index before switching serving state.

#### Scenario: Download and enable a variant

- **GIVEN** a client has saved a custom mirror with `PATCH /v1/host/config`
- **WHEN** the client downloads a variant through `POST /v1/models/downloads` and enables it with `POST /v1/models/switch`
- **THEN** the download uses the selected mirror and the serving model changes only after the shadow index is ready

### Requirement: Non-Destructive Candidate Model Probe

`POST /v1/models/probe` MUST load installed candidate weights and run sample inference on the requested engine without changing the serving model, index, or installed weights. The result MUST include provider, timing, dimensions, L2 norm, and copyable diagnostics. Invalid selections MUST return HTTP 400 and missing weights MUST return HTTP 404.

#### Scenario: Probe installed weights

- **GIVEN** the candidate variant is installed
- **WHEN** a client probes it on CPU
- **THEN** the response reports load and inference metrics and serving state remains unchanged

### Requirement: Safe Non-Active Model Weight Deletion

`POST /v1/models/delete` MUST remove only downloaded, non-active weights. It MUST return HTTP 409 for active weights or weights targeted by a running `model_download` or `model_switch` operation. If the request omits the variant, any running download or switch targeting the model MUST block deletion. It MUST return HTTP 404 for missing weights and HTTP 400 for an invalid model or variant.

#### Scenario: Protect active weights

- **GIVEN** one active variant and one installed non-active variant
- **WHEN** a client requests deletion of each variant
- **THEN** Runtime rejects active deletion with HTTP 409 and reports the removed paths for the non-active variant

#### Scenario: Protect in-flight weights

- **GIVEN** a download or switch targeting a candidate variant is running
- **WHEN** a client requests deletion of that variant or omits the variant for that model
- **THEN** Runtime returns HTTP 409 with `operation_in_progress` until the operation ends, after which normal deletion rules apply
