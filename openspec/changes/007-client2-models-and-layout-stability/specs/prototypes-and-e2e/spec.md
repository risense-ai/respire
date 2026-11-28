## ADDED Requirements

### Requirement: Fixed Desktop Geometry and Floating Toast Notices

All 16 Client2 desktop views SHALL keep page and window scroll height stable during option changes. Notices MUST float outside document flow.

#### Scenario: Show and dismiss a notice after changing options

- **GIVEN** any of the 16 desktop views at a fixed window size
- **WHEN** a user changes an option and a notice appears and disappears
- **THEN** the outer scroll height and card positions remain constant

### Requirement: Two-Level Models Screen and Aligned Setup Wizard

Models MUST show four family cards, a fixed two-row variant grid, a mirror selector with inline custom URL, and one engine and action dock. The probe MUST open a modal with metrics, copyable diagnostics, and a reporting link. The Setup Wizard MUST show `Granite 311M R2 (quint8_avx2)` as its preset.

#### Scenario: Open two families and test a candidate

- **GIVEN** the user is on Models
- **WHEN** the user changes from a two-variant family to an eight-variant family and tests installed weights
- **THEN** the variant region retains its height and the modal shows the probe result

### Requirement: Overview and Model Activity Cards

Overview MUST show the active model and index coverage in a fixed-height card. Models MUST show fixed-height cards for live inference and the switch pipeline, using structured fields and SSE progress. Snapshot JSON MUST be available through a copy action only.

#### Scenario: Follow a model switch

- **GIVEN** a model switch is running
- **WHEN** Models receives an operation event
- **THEN** progress and the activity log update within the existing card bounds, and the snapshot can be copied

### Requirement: Mock Runtime Probe and Delete Parity

The Mock Runtime MUST implement the OpenAPI probe and delete endpoints. `E2E-008` MUST assert probe success, preservation of the active model, rejection of active weight deletion, and successful non-active weight deletion.

#### Scenario: Run the mock acceptance flow

- **GIVEN** the `rich-1k` scenario
- **WHEN** `E2E-008` probes the default model and attempts both deletions
- **THEN** the probe succeeds, the active assets remain selected, active deletion returns HTTP 409, and non-active deletion returns HTTP 200
