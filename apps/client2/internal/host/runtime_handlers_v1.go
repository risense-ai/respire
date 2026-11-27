package host

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"strings"

	runtimesdk "github.com/risense-ai/respire/packages/runtime-sdk-go"
)

// Only contract-covered operations enter here. The frozen dispatcher retains
// diary search, session import, memory tree/chain/share, grants and account custody.
func (m *Manager) executeV1Operation(ctx context.Context, name, id string, input json.RawMessage) (json.RawMessage, bool, error) {
	capability := "openapi.v1"
	switch name {
	case "sync", "space_list", "space_create", "space_use", "space_remove", "model_progress", "model_cancel":
	case "memory_backup", "memory_import":
		capability = "backups.v1"
	default:
		return nil, false, nil
	}
	var args struct {
		Name string `json:"name"`
		Path string `json:"path"`
		ID   string `json:"id"`
	}
	if len(input) > 0 {
		if err := json.Unmarshal(input, &args); err != nil {
			return nil, true, err
		}
	}
	if (name == "memory_backup" || name == "memory_import") && filepath.Ext(args.Path) != ".rsrs-backup" {
		// JSON exports/imports and raw legacy DB backups have no V1 equivalent.
		return nil, false, nil
	}
	if name == "memory_import" {
		// V1 only stages bytes. The native confirmation helper is not exposed
		// by the current Runtime; never claim that uploading restores a library.
		return nil, true, errors.New("Runtime backup restoration requires a native owner confirmation helper; archive restoration is unavailable in this build")
	}
	if (name == "space_use" || name == "space_remove") && !strings.HasPrefix(args.Name, "v1:") {
		return nil, false, nil
	}
	client, supported, err := m.runtimeSDK(ctx, capability)
	if err != nil {
		if name == "space_list" && IsUnavailable(err) {
			return nil, false, nil
		}
		return nil, supported, err
	}
	if !supported {
		if name == "memory_backup" || name == "memory_import" || strings.HasPrefix(args.Name, "v1:") {
			return nil, true, fmt.Errorf("Runtime does not advertise %s", capability)
		}
		return nil, false, nil
	}
	if name == "space_create" {
		legacy, err := m.runtimeStatusLegacy(ctx)
		if err != nil {
			return nil, true, err
		}
		var status struct {
			Summary struct {
				DataDir          string `json:"data_dir"`
				RemoteConfigured *bool  `json:"remote_configured"`
				Session          struct {
					Addr     string `json:"addr"`
					HasToken bool   `json:"has_token"`
				} `json:"session"`
			} `json:"summary"`
		}
		if err := json.Unmarshal(legacy, &status); err != nil {
			return nil, true, err
		}
		if status.Summary.RemoteConfigured == nil {
			return nil, true, errors.New("Runtime did not report whether device-space selection is reversible")
		}
		if status.Summary.DataDir == "" {
			return nil, true, errors.New("Runtime did not identify the original space directory")
		}
		if !samePath(status.Summary.DataDir, m.DataRoot) || *status.Summary.RemoteConfigured || status.Summary.Session.Addr != "" || status.Summary.Session.HasToken {
			return nil, false, nil // Account-backed spaces retain the native custody workflow.
		}
	}
	if name != "model_progress" && name != "model_cancel" {
		if err := m.markV1Operation(id, ""); err != nil {
			return nil, true, err
		}
	}
	var raw json.RawMessage
	switch name {
	case "sync":
		raw, err = sdkJSON(client.TriggerSync(ctx, runtimesdk.TriggerSyncRequest{}, m.authorizeRuntime))
	case "model_progress", "model_cancel":
		response, requestErr := client.GetOperation(ctx, args.ID, m.authorizeRuntime)
		if requestErr == nil && response.StatusCode == http.StatusNotFound {
			response.Body.Close()
			return nil, false, nil // This ID belongs to the frozen legacy model worker.
		}
		raw, err = sdkJSON(response, requestErr)
		if err == nil {
			err = m.markV1Operation(id, args.ID)
		}
		if err == nil && name == "model_cancel" {
			raw, err = m.RuntimeOperation(ctx, args.ID, true)
		}
	case "space_list":
		return m.listSpacesV1(ctx, client)
	case "space_create":
		raw, err = sdkJSON(client.CreateSpace(ctx, runtimesdk.CreateSpaceRequest{Name: args.Name}, m.authorizeRuntime))
		if err == nil {
			var space runtimesdk.SpaceItem
			if err = json.Unmarshal(raw, &space); err == nil {
				active := true
				raw, err = sdkJSON(client.UpdateSpace(ctx, space.Id, runtimesdk.UpdateSpaceRequest{Active: &active}, m.authorizeRuntime))
			}
			if err != nil {
				return raw, true, &unconfirmedError{id, err}
			}
		}
	case "space_use", "space_remove":
		spaceID := strings.TrimPrefix(args.Name, "v1:")
		if spaceID == "main" && name == "space_remove" {
			return nil, true, errors.New("The original space cannot be deleted")
		}
		if name == "space_use" {
			active := true
			raw, err = sdkJSON(client.UpdateSpace(ctx, spaceID, runtimesdk.UpdateSpaceRequest{Active: &active}, m.authorizeRuntime))
		} else {
			raw, err = sdkJSON(client.DeleteSpace(ctx, spaceID, m.authorizeRuntime))
		}
	case "memory_backup", "memory_import":
		raw, err = m.backupV1(ctx, client, args.Path, name == "memory_import")
	}
	if err != nil {
		var rejected *RuntimeHTTPError
		uncertain := !errors.As(err, &rejected)
		if rejected != nil {
			uncertain = strings.HasSuffix(rejected.Code, "_outcome_unknown")
		}
		if name != "space_list" && name != "model_progress" && uncertain {
			return raw, true, &unconfirmedError{id, err}
		}
		return raw, true, err
	}
	if name == "sync" {
		var accepted runtimesdk.OperationAccepted
		if err := json.Unmarshal(raw, &accepted); err != nil {
			return nil, true, &unconfirmedError{id, err}
		}
		if accepted.OperationId == "" {
			return nil, true, &unconfirmedError{id, errors.New("Runtime operation receipt has no ID")}
		}
		m.mu.Lock()
		if job := m.jobs[id]; job != nil {
			job.RuntimeID = accepted.OperationId
			if err := m.persistOperationsLocked(); err != nil {
				m.mu.Unlock()
				return nil, true, err
			}
		}
		m.mu.Unlock()
		_, stream, err := m.runtimeSDK(ctx, "sse.operations.v1")
		if err == nil {
			err = m.waitRuntimeOperation(ctx, accepted, stream, nil)
		}
		if err != nil {
			var terminal *runtimeTerminalError
			if errors.As(err, &terminal) {
				return nil, true, err
			}
			return nil, true, &unconfirmedError{id, err}
		}
	}
	result, err := desktopEnvelope(name, raw)
	return result, true, err
}

func (m *Manager) markV1Operation(id, runtimeID string) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	if job := m.jobs[id]; job != nil {
		job.RuntimeV1, job.host = true, false
		if runtimeID != "" {
			job.RuntimeID = runtimeID
		}
		if job.write {
			return m.persistOperationsLocked()
		}
	}
	return nil
}

func (m *Manager) recoverV1Operation(ctx context.Context, job Operation) (json.RawMessage, error) {
	if job.RuntimeID == "" {
		return nil, &unconfirmedError{job.ID, errors.New("Runtime returned no operation ID; inspect current state before retrying")}
	}
	_, stream, err := m.runtimeSDK(ctx, "sse.operations.v1")
	if err == nil {
		err = m.waitRuntimeOperation(ctx, runtimesdk.OperationAccepted{OperationId: job.RuntimeID}, stream, nil)
	}
	if err != nil {
		var terminal *runtimeTerminalError
		if errors.As(err, &terminal) {
			if job.Name == "model_cancel" && terminal.state == "cancelled" {
				return desktopEnvelope(job.Name, map[string]string{"operation_id": job.RuntimeID, "state": "cancelled"})
			}
			return nil, err
		}
		return nil, &unconfirmedError{job.ID, err}
	}
	return desktopEnvelope(job.Name, map[string]string{"operation_id": job.RuntimeID, "state": "completed"})
}

func (m *Manager) listSpacesV1(ctx context.Context, client *runtimesdk.Client) (json.RawMessage, bool, error) {
	raw, err := sdkJSON(client.ListSpaces(ctx, m.authorizeRuntime))
	if err != nil {
		return nil, true, err
	}
	var spaces runtimesdk.SpaceListResponse
	if err := json.Unmarshal(raw, &spaces); err != nil {
		return nil, true, err
	}
	legacy, err := m.desktopProfileList(true)
	if err != nil {
		return nil, true, err
	}
	var envelope struct {
		Summary struct {
			Spaces []map[string]any `json:"spaces"`
		} `json:"summary"`
	}
	if err := json.Unmarshal(legacy, &envelope); err != nil {
		return nil, true, err
	}
	activeChild := false
	for _, space := range spaces.Spaces {
		activeChild = activeChild || space.Active != nil && *space.Active
	}
	originalFound := false
	for _, row := range envelope.Summary.Spaces {
		if row["current"] == true {
			row["id"], row["current"], row["is_main"] = "main", !activeChild, true
			originalFound = true
		}
	}
	if !originalFound {
		envelope.Summary.Spaces = append(envelope.Summary.Spaces, map[string]any{"id": "main", "name": "main", "role": "owner", "current": !activeChild, "is_main": true})
	}
	for _, space := range spaces.Spaces {
		envelope.Summary.Spaces = append(envelope.Summary.Spaces, map[string]any{
			"id": space.Id, "name": space.Name, "role": space.Role, "current": space.Active != nil && *space.Active,
		})
	}
	result, err := desktopEnvelope("spaces", envelope.Summary)
	return result, true, err
}

func (m *Manager) backupV1(ctx context.Context, client *runtimesdk.Client, path string, restore bool) (json.RawMessage, error) {
	httpClient := *m.localClient
	httpClient.Timeout = 0
	client, err := runtimesdk.NewClient(m.Endpoint, runtimesdk.WithHTTPClient(&httpClient))
	if err != nil {
		return nil, err
	}
	if restore {
		file, err := os.Open(path)
		if err != nil {
			return nil, err
		}
		defer file.Close()
		return sdkJSON(client.ImportBackupWithBody(ctx, nil, "application/octet-stream", file, m.authorizeRuntime))
	}
	response, err := client.ExportBackup(ctx, nil, m.authorizeRuntime)
	if err != nil {
		return nil, err
	}
	if response.StatusCode != http.StatusOK {
		return sdkJSON(response, nil)
	}
	defer response.Body.Close()
	file, err := os.CreateTemp(filepath.Dir(path), ".backup-*")
	if err != nil {
		return nil, err
	}
	defer os.Remove(file.Name())
	_, copyErr := io.Copy(file, response.Body)
	if err := errors.Join(copyErr, file.Sync(), file.Close()); err != nil {
		return nil, err
	}
	if err := os.Rename(file.Name(), path); err != nil {
		return nil, fmt.Errorf("Publish Runtime backup: %w", err)
	}
	return json.Marshal(map[string]string{"path": path})
}
