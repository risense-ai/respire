//go:build linux || windows || (darwin && cgo)

package host

import "respire-client2/internal/i18n"

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"path/filepath"
	"reflect"
)

const (
	migrateLegacyMethod = "control.migration.profile"
	migrateResultMethod = "control.migration.result"
)

type legacyMigrationOperation struct {
	Schema      int                 `json:"schema_version"`
	OperationID string              `json:"operation_id"`
	Key         string              `json:"key"`
	Root        string              `json:"installation_root"`
	Profile     string              `json:"profile"`
	Identity    runtimeAuthIdentity `json:"identity"`
	SourceID    string              `json:"source_id"`
	Account     string              `json:"account"`
	Fingerprint string              `json:"fingerprint"`
}

func (o legacyMigrationOperation) fingerprint() (string, error) {
	var wire bytes.Buffer
	encoder := json.NewEncoder(&wire)
	encoder.SetEscapeHTML(false)
	if err := encoder.Encode([]any{1, migrateLegacyMethod, o.Root, o.Profile, o.Identity, o.SourceID, o.Account}); err != nil {
		return "", err
	}
	// The Rust fingerprint uses original UTF-8 for separators and preserves
	// encoded backslashes in paths.
	encoded := bytes.TrimSpace(wire.Bytes())
	canonical := make([]byte, 0, len(encoded))
	for i := 0; i < len(encoded); i++ {
		if encoded[i] != '\\' || i+1 >= len(encoded) {
			canonical = append(canonical, encoded[i])
			continue
		}
		switch {
		case bytes.HasPrefix(encoded[i:], []byte(`\u2028`)):
			canonical = append(canonical, "\u2028"...)
			i += 5
		case bytes.HasPrefix(encoded[i:], []byte(`\u2029`)):
			canonical = append(canonical, "\u2029"...)
			i += 5
		default:
			canonical = append(canonical, encoded[i:i+2]...)
			i++
		}
	}
	return controlDigest(canonical), nil
}

type legacyMigrationResult struct {
	OperationID string              `json:"operation_id"`
	Fingerprint string              `json:"fingerprint"`
	Identity    runtimeAuthIdentity `json:"identity"`
	Outcome     string              `json:"outcome"`
	Details     json.RawMessage     `json:"details"`
}

func (m *Manager) exchangeLegacyMigration(ctx context.Context, control runtimeAccountControl, operation legacyMigrationOperation,
	method string, material *legacyMigrationMaterial) (legacyMigrationResult, error) {
	var result legacyMigrationResult
	requestID, err := controlUUID()
	if err != nil {
		return result, err
	}
	body, err := json.Marshal(struct {
		Schema   int                      `json:"schema_version"`
		Binding  runtimeControlBinding    `json:"binding"`
		Identity runtimeAuthIdentity      `json:"identity"`
		Original legacyMigrationOperation `json:"original"`
		Material *legacyMigrationMaterial `json:"material,omitempty"`
	}{1, control.binding, operation.Identity, operation, material})
	if err != nil {
		return result, err
	}
	defer clear(body)
	reply, exchangeErr := m.exchangeRuntimeControl(ctx, control.image, control.port, control.grant,
		runtimeControlRequest{V: 1, ID: requestID, Method: method, Args: []string{string(body)},
			OperationID: operation.OperationID, IdempotencyKey: operation.Key})
	rejected := reply.Code == "migration_schema_rejected" && !reply.OK && reply.Exit == 1 && reply.Error == ""
	if exchangeErr != nil && !rejected {
		return result, exchangeErr
	}
	if (rejected && reply.DataDir == "") || (reply.DataDir != "" && !samePath(reply.DataDir, operation.Profile)) {
		return result, errors.New(i18n.Message22decffc0515f39a)
	}
	var envelope struct {
		Command string
		Status  string
		Summary json.RawMessage
		Details json.RawMessage
		Errors  []json.RawMessage
		Actions []json.RawMessage
		Related []json.RawMessage
		Items   []struct{ Name, Status, Value string }
	}
	if json.Unmarshal(reply.Envelope, &envelope) != nil || envelope.Command != "migration-control" ||
		string(envelope.Details) != "null" || len(envelope.Errors)+len(envelope.Actions)+len(envelope.Related) != 0 ||
		len(envelope.Items) != 2 {
		return result, errors.New(i18n.Message383e8f2981cd0242)
	}
	ready, locked := false, false
	for _, item := range envelope.Items {
		ready = ready || (item.Name == "control_ready" && item.Status == "ok" && item.Value == "true")
		locked = locked || (item.Name == "store_ready" && item.Status == "skip" && item.Value == "false")
	}
	if !ready || !locked {
		return result, errors.New(i18n.Messagee3018394d0ef1891)
	}
	current, err := deviceProfileIdentity(operation.Profile)
	if err != nil || !reflect.DeepEqual(current, operation.Identity) {
		return result, errors.Join(errors.New(i18n.Message6ffc25b6c4215b53), err)
	}
	if rejected {
		var original legacyMigrationOperation
		if envelope.Status != "fail" || json.Unmarshal(envelope.Summary, &original) != nil || !reflect.DeepEqual(original, operation) {
			return result, errors.New(i18n.Message629f14e7f7570f76)
		}
		return legacyMigrationResult{OperationID: operation.OperationID, Fingerprint: operation.Fingerprint,
			Identity: operation.Identity, Outcome: "schema_rejected"}, nil
	}
	if envelope.Status != "ok" || json.Unmarshal(envelope.Summary, &result) != nil ||
		result.OperationID != operation.OperationID || result.Fingerprint != operation.Fingerprint ||
		!reflect.DeepEqual(result.Identity, operation.Identity) {
		return result, errors.New(i18n.Messagee3cbcfa1da05bb46)
	}
	if result.Outcome == "credentials_required" {
		return result, nil
	}
	if result.Outcome == "already_migrated" && method != migrateLegacyMethod {
		return result, errors.New(i18n.Messagec0d7a4c296eabe56)
	}
	return result, validateTerminalLegacyMigration(result, operation)
}

func validateTerminalLegacyMigration(result legacyMigrationResult, operation legacyMigrationOperation) error {
	if result.OperationID != operation.OperationID || result.Fingerprint != operation.Fingerprint ||
		!reflect.DeepEqual(result.Identity, operation.Identity) {
		return errors.New(i18n.Messageb837c8fa820b08e1)
	}
	switch result.Outcome {
	case "schema_rejected":
		if len(result.Details) != 0 && string(result.Details) != "null" {
			return errors.New(i18n.Message190ea18bb6982976)
		}
	case "completed":
		var details struct {
			State     string `json:"state"`
			Directory string `json:"dir"`
			Account   string `json:"account"`
			Preserved bool   `json:"original_preserved"`
		}
		if json.Unmarshal(result.Details, &details) != nil || details.State != "migrated" || !details.Preserved ||
			details.Account != operation.Account || !samePath(details.Directory, filepath.Join(operation.Root, "accounts", operation.Account)) {
			return errors.New(i18n.Messageff5a7b2ffdae4a53)
		}
	case "already_migrated":
		var details struct{ State string }
		if json.Unmarshal(result.Details, &details) != nil || details.State != "already_migrated" {
			return errors.New(i18n.Message3220349ba1c52b1a)
		}
	default:
		return errors.New(i18n.Message110bb9236f6c83ad)
	}
	return nil
}
