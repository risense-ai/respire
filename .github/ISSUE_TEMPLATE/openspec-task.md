---
name: OpenSpec Feature / Refactor Task
about: Submit a task governed by an OpenSpec Change and Milestone DAG
title: "[MX][00X] <Concise English summary>"
labels: ["spec:proposed"]
---

## Linked OpenSpec Metadata (Required)

- **OpenSpec Change ID**: `00X-...`
- **Target Milestone**: `MX: ...`
- **Spec Section / Scenario**: `openspec/changes/.../specs/.../spec.md`
- **Contract Path** (if API): `contracts/runtime-api/openapi.yaml`
- **HTML Prototype Path** (if UI): `prototypes/pages/...` or `N/A`
- **E2E Case ID**: `E2E-...`
- **Depends On**: `#...` or `None`

## Definition of Done

- [ ] The Change and this Milestone Issue received `spec:approved` before implementation
- [ ] Implementation written strictly in English (no CJK outside UI i18n paths)
- [ ] Meets all linked OpenSpec scenarios and E2E checks in `test-cases/`
- [ ] Passes `node process/scripts/validate-openspec.mjs` and `node scripts/check-guardrails.mjs`
- [ ] Verified independently by a read-only Verifier using `process/roles/verifier.md`
