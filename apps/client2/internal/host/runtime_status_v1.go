package host

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	runtimesdk "github.com/risense-ai/respire/packages/runtime-sdk-go"
	"respire-client2/internal/i18n"
	"time"
)

func (m *Manager) RuntimeStatus(ctx context.Context) (json.RawMessage, error) {
	client, supported, err := m.runtimeSDK(ctx, "openapi.v1")
	if err != nil {
		return nil, err
	}
	if !supported {
		return m.runtimeStatusLegacy(ctx)
	}
	raw, err := sdkJSON(client.GetLightStatus(ctx, nil, m.authorizeRuntime))
	if err != nil {
		return nil, err
	}
	var light runtimesdk.LightStatusResponse
	if err := json.Unmarshal(raw, &light); err != nil {
		return nil, err
	}
	syncRaw, err := sdkJSON(client.GetSyncStatus(ctx, m.authorizeRuntime))
	if err != nil {
		return nil, err
	}
	var syncStatus runtimesdk.SyncStatusResponse
	if err := json.Unmarshal(syncRaw, &syncStatus); err != nil {
		return nil, err
	}
	// Detailed session/inference fields are not represented by status/light.
	legacy, err := m.runtimeStatusLegacy(ctx)
	if err != nil {
		return nil, err
	}
	var envelope map[string]any
	if err := json.Unmarshal(legacy, &envelope); err != nil {
		return nil, err
	}
	summary, ok := envelope["summary"].(map[string]any)
	if !ok {
		return nil, errors.New("Runtime status summary is missing")
	}
	summary["local_alive"], summary["indexed_count"], summary["diary_count"] = light.MemoryCount, light.IndexedCount, light.DiaryCount
	summary["space_id"] = light.SpaceId
	live, ok := summary["sync_live"].(map[string]any)
	if !ok {
		live = map[string]any{}
		summary["sync_live"] = live
	}
	live["phase"], live["pending"] = syncStatus.State, syncStatus.PendingOutbox
	return json.Marshal(envelope)
}

func (m *Manager) runtimeStatusLegacy(ctx context.Context) (json.RawMessage, error) {
	scope, err := m.requestScope(ctx)
	if err != nil {
		return nil, err
	}
	id := "status-" + fmt.Sprint(time.Now().UnixNano())
	reply, err := m.rpcResponse(ctx, "cli.exec", []string{"--json", "status"}, id, scope)
	if err != nil {
		return m.inspectResult(ctx, id, scope, err)
	}
	if reply.Scope == nil || *reply.Scope != *scope || reply.OperationID != id {
		return nil, &unconfirmedError{id, errors.New(i18n.Messagefb9a457abcc23607)}
	}
	var status struct {
		Command string `json:"command"`
		Status  string `json:"status"`
		Summary struct {
			CredentialState string `json:"credential_state"`
			StoreReady      *bool  `json:"store_ready"`
		} `json:"summary"`
		Errors []string `json:"errors"`
	}
	// A locked store is a valid read-only status, but other RPC warnings still fail.
	if !reply.OK && reply.Exit == 2 && reply.Code == "" && reply.Error == "" &&
		json.Unmarshal(reply.Envelope, &status) == nil && status.Command == "status" &&
		status.Status == "warn" && status.Summary.CredentialState == "required" &&
		status.Summary.StoreReady != nil && !*status.Summary.StoreReady && len(status.Errors) == 0 {
		return reply.Envelope, nil
	}
	return reply.Envelope, responseError(reply)
}
