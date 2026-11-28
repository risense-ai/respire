# Client2 Models and Layout Design

The design spans the monorepo's [Runtime API contract](../../../contracts/runtime-api/openapi.yaml), [Runtime](../../../apps/runtime/), [Client2](../../../apps/client2/), and [prototypes](../../../prototypes/).

## Desktop Geometry (`CARD-022`, #128)

All 16 desktop views use fixed container and card geometry. Switching options changes only the contents of bounded regions, with internal scrolling where needed. The page and window scroll height stay constant. The `#notice` toast uses `position: fixed` outside document flow, so showing or dismissing it causes no vertical layout shift.

## Catalog and Weight Lifecycle (`CARD-023`, #129)

Retire `bge-m3-distill-8l` from selection. The selectable ONNX catalog contains exactly these families:

| ID | Weight repository | Dimensions | Context tokens | Default variant |
| --- | --- | ---: | ---: | --- |
| `granite-311m-r2` | `ibm-granite/granite-embedding-311m-multilingual-r2` | 768 | 32768 | `quint8_avx2` (default active recommendation) |
| `granite-97m-r2` | `ibm-granite/granite-embedding-97m-multilingual-r2` | 384 | 32768 | `quint8_avx2` |
| `m3` | `Xenova/bge-m3` | 1024 | 512 | `quantized` |
| `bge-base-zh-v1.5` | `Xenova/bge-base-zh-v1.5` | 768 | 512 | `quantized` |

| Family | Variant | ONNX path | Bytes |
| --- | --- | --- | ---: |
| `granite-311m-r2` | `quint8_avx2` | `onnx/model_quint8_avx2.onnx` | 313421909 |
| `granite-311m-r2` | `standard` | `onnx/model.onnx` | 1247170481 |
| `granite-97m-r2` | `quint8_avx2` | `onnx/model_quint8_avx2.onnx` | 98247878 |
| `granite-97m-r2` | `standard` | `onnx/model.onnx` | 390004608 |
| `m3` | `quantized` | `onnx/model_quantized.onnx` | 569694530 |
| `m3` | `q4f16` | `onnx/model_q4f16.onnx` | 699932840 |
| `m3` | `fp16` | `onnx/model_fp16.onnx` | 1133992936 |
| `m3` | `int8` | `onnx/model_int8.onnx` | 568456694 |
| `m3` | `uint8` | `onnx/model_uint8.onnx` | 568456765 |
| `m3` | `q4` | `onnx/model_q4.onnx` | 1248237611 |
| `m3` | `bnb4` | `onnx/model_bnb4.onnx` | 1229364419 |
| `m3` | `fp32` | `onnx/model.onnx` | 607298 |
| `bge-base-zh-v1.5` | `quantized` | `onnx/model_quantized.onnx` | 102868746 |
| `bge-base-zh-v1.5` | `q4f16` | `onnx/model_q4f16.onnx` | 81549931 |
| `bge-base-zh-v1.5` | `fp16` | `onnx/model_fp16.onnx` | 203679052 |
| `bge-base-zh-v1.5` | `int8` | `onnx/model_int8.onnx` | 102407793 |
| `bge-base-zh-v1.5` | `uint8` | `onnx/model_uint8.onnx` | 102407824 |
| `bge-base-zh-v1.5` | `q4` | `onnx/model_q4.onnx` | 120342974 |
| `bge-base-zh-v1.5` | `bnb4` | `onnx/model_bnb4.onnx` | 115035086 |
| `bge-base-zh-v1.5` | `fp32` | `onnx/model.onnx` | 406953171 |

The `m3` `fp32` variant also requires `onnx/model.onnx_data` (2266820608 bytes). The catalog reports `total_size_bytes` of 2267427906 for both files, which is also the space freed when deleting the variant.

Download and Enable are separate steps. Enable builds a shadow index while the serving model and index remain active. Commit the switch only after the replacement is ready. There is no disable-all-embeddings control.

`POST /v1/models/probe` loads the selected model, variant, and engine and runs sample inference without changing serving state. The response carries load and inference timing, dimensions, L2 norm, provider, and a copyable diagnostic log. `POST /v1/models/delete` returns HTTP 409 for active weights or when a running `model_download` or `model_switch` targets that model and variant. When the request omits the variant, any running download or switch targeting the model blocks deletion.

`DownloadModelRequest.mirror` accepts a custom URL or a preset. Persist the selection as `model_mirror` via `PATCH /v1/host/config`.

## Models, Overview, and Onboarding (`CARD-024`, #130 and `CARD-025`, #131)

- Use two fixed-height cards at the top of Models. Active Model & Live Inference Activity shows the active model, variant, engine, latency, foreground and background queues, and index coverage. Model Tasks & Switch Pipeline shows integrity, worker and lock state, SSE progress, and an activity log. Provide `Copy Snapshot JSON` without displaying a raw JSON dump.
- Level 1 shows four model cards with a top-right Download Mirror selector for `auto`, `https://hf-mirror.com`, `https://hf-mirror.net`, `https://huggingface.co`, and `custom`. The custom option uses an inline URL field.
- Level 2 uses a fixed-height, two-row quantization grid for both two-variant and eight-variant families. One row contains the CPU, GPU, and NPU engine selector and the Download, Enable, Load & Inference Test, and Delete controls. Keep labels concise.
- The probe modal displays metrics and offers a diagnostic-log copy action and a GitHub Issue reporting link.
- Overview shows fixed-height Model & Index Status with a link to Models. The Onboarding Wizard uses `granite-311m-r2` with `quint8_avx2`.
- The HTML prototypes use `tokens.css` for layout and interaction references. Production `apps/client2` keeps its native theme colors and styling.
