# 007 Client2 Models and Layout Stability

## Why

Changing options or showing status messages can move desktop controls. Model management also needs to expose the exact ONNX weights selected for download, testing, and activation.

## What Changes

- Stabilize all 16 desktop views and display notices in a floating overlay ([#128](https://github.com/risense-ai/respire/issues/128), `CARD-022`).
- Curate four ONNX model families, recommend `granite-311m-r2` with `quint8_avx2` by default, and add variant-aware downloads, candidate probes, and safe weight deletion ([#129](https://github.com/risense-ai/respire/issues/129), `CARD-023`).
- Redesign Models around family and variant selection, and align the Onboarding Wizard with the default preset ([#130](https://github.com/risense-ai/respire/issues/130), `CARD-024`).
- Show model and index status on Overview and split live inference activity and switch progress into two Models cards ([#131](https://github.com/risense-ai/respire/issues/131), `CARD-025`).

## Impact

The Runtime OpenAPI contract evolves additively. The mock, prototypes, and acceptance scenarios provide references for implementation in `risense-ai/respire` after approval.
