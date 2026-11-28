package host

import (
	"context"
	"encoding/json"
	"errors"
	"slices"

	runtimesdk "github.com/risense-ai/respire/packages/runtime-sdk-go"
)

// Variant is deliberately absent from the download contract. Only the pinned
// default weight can use it; baseline engine bundles and other weights stay local.
func (m *Manager) StartRuntimeModelDownload(ctx context.Context, model, variant, mirror string) (ModelSwitchReceipt, bool, error) {
	definition, err := modelDefinitionByID(model)
	if err != nil {
		return ModelSwitchReceipt{}, true, err
	}
	if variant != definition.Variants[0].Name {
		return ModelSwitchReceipt{}, false, nil
	}
	mirror, err = NormalizeModelMirror(m.modelMirrorSetting(mirror))
	if err != nil {
		return ModelSwitchReceipt{}, true, err
	}
	if mirror != "auto" && !slices.Contains(modelMirrors, mirror) {
		return ModelSwitchReceipt{}, false, nil // Custom origins are not accepted by the V1 downloader.
	}
	client, supported, err := m.runtimeSDK(ctx, "model.managed_download")
	if IsUnavailable(err) {
		return ModelSwitchReceipt{}, false, nil
	}
	if err != nil || !supported {
		return ModelSwitchReceipt{}, supported, err
	}
	_, stream, err := m.runtimeSDK(ctx, "sse.operations.v1")
	if err != nil {
		return ModelSwitchReceipt{}, true, err
	}
	engine := "cpu"
	var selectedMirror *string
	if mirror != "auto" {
		selectedMirror = &mirror
	}
	raw, err := sdkJSON(client.StartModelDownload(ctx, runtimesdk.DownloadModelRequest{
		ModelId: model, Engine: &engine, Mirror: selectedMirror,
	}, m.authorizeRuntime))
	if err != nil {
		return ModelSwitchReceipt{}, true, err
	}
	var accepted runtimesdk.OperationAccepted
	if err := json.Unmarshal(raw, &accepted); err != nil {
		return ModelSwitchReceipt{}, true, err
	}
	if accepted.OperationId == "" {
		return ModelSwitchReceipt{}, true, errors.New("Runtime download receipt has no operation ID")
	}
	return ModelSwitchReceipt{OperationAccepted: &accepted, Stream: stream}, true, err
}

func (m *Manager) ActiveModelAssets(ctx context.Context) (json.RawMessage, error) {
	client, supported, err := m.runtimeSDK(ctx, "model.active_assets")
	if err != nil {
		return nil, err
	}
	if !supported {
		return m.runtimeRPC(ctx, "assets.status", nil)
	}
	return sdkJSON(client.GetActiveModelAssets(ctx, m.authorizeRuntime))
}

func (m *Manager) modelCatalogV1(ctx context.Context) (json.RawMessage, bool, error) {
	client, supported, err := m.runtimeSDK(ctx, "model.catalog.v2")
	if err != nil || !supported {
		return nil, supported, err
	}
	raw, err := sdkJSON(client.GetModelCatalog(ctx, m.authorizeRuntime))
	if err != nil {
		return nil, true, err
	}
	var catalog runtimesdk.ModelCatalogResponse
	if err := json.Unmarshal(raw, &catalog); err != nil {
		return nil, true, err
	}
	catalog.Models = slices.DeleteFunc(catalog.Models, func(model runtimesdk.ModelCatalogEntry) bool {
		_, err := modelDefinitionByID(model.Id)
		return err != nil
	})
	// The V1 catalog has no engine/variant selection fields. Preserve those
	// legacy-only details until the contract represents them.
	legacy, err := m.runtimeRPC(ctx, "model.catalog", nil)
	if err != nil {
		return nil, true, err
	}
	var envelope map[string]any
	if err := json.Unmarshal(legacy, &envelope); err != nil {
		return nil, true, err
	}
	envelope["catalog_v1"] = catalog
	encoded, err := json.Marshal(envelope)
	return encoded, true, err
}
