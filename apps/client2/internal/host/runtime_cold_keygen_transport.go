//go:build linux || windows || (darwin && cgo)

package host

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"path/filepath"
)

type runtimeControlMode uint8

const (
	runtimeControlLegacy runtimeControlMode = iota
	runtimeControlColdKeygen

	coldKeygenCapabilityMethod = "control.auth.cold_keygen_capability"
)

func (mode runtimeControlMode) flag(req runtimeControlRequest) (string, error) {
	switch mode {
	case runtimeControlLegacy:
		if req.Method != coldKeygenCapabilityMethod {
			return "--host-control-internal", nil
		}
	case runtimeControlColdKeygen:
		if req.V != 1 || !hostOperationID.MatchString(req.ID) || len(req.Args) != 1 {
			break
		}
		switch req.Method {
		case coldKeygenCapabilityMethod:
			if _, exact := coldKeygenObject([]byte(req.Args[0]), "schema_version", "binding"); exact && req.OperationID == "" && req.IdempotencyKey == "" {
				return "--host-cold-keygen-internal", nil
			}
		case "control.auth.keygen", "control.auth.result":
			fields, exact := coldKeygenObject([]byte(req.Args[0]), "schema_version", "binding", "identity", "force", "material")
			identity, absent := coldKeygenObject(fields["identity"], "kind")
			if exact && absent && coldKeygenString(identity, "kind") == "absent" && bytes.Equal(bytes.TrimSpace(fields["force"]), []byte("false")) &&
				hostOperationID.MatchString(req.OperationID) && req.OperationID == req.IdempotencyKey {
				return "--host-cold-keygen-internal", nil
			}
		}
	}
	return "", errors.New("Unsupported private Runtime control mode or request")
}

type coldKeygenRequestParams struct {
	Schema  int                   `json:"schema_version"`
	Binding runtimeControlBinding `json:"binding"`
}

func newColdKeygenProbe(binding runtimeControlBinding) (runtimeControlRequest, error) {
	return newColdKeygenRequest(coldKeygenCapabilityMethod, "", coldKeygenRequestParams{Schema: 1, Binding: binding})
}

func newColdKeygenOperation(binding runtimeControlBinding, op string, resultOnly bool, material runtimeRegisterMaterial) (runtimeControlRequest, error) {
	if !hostOperationID.MatchString(op) || material.Account != offlineKeygenAlias(op) || material.Backend != runtimeLoginBackend() ||
		material.Super == "" || material.Salt == "" || material.Wrapped == "" || material.Nonce == "" {
		return runtimeControlRequest{}, errors.New("Invalid private Runtime keygen material or operation")
	}
	method := "control.auth.keygen"
	if resultOnly {
		method = "control.auth.result"
	}
	return newColdKeygenRequest(method, op, struct {
		coldKeygenRequestParams
		Identity runtimeAuthIdentity     `json:"identity"`
		Force    bool                    `json:"force"`
		Material runtimeRegisterMaterial `json:"material"`
	}{coldKeygenRequestParams{Schema: 1, Binding: binding}, runtimeAuthIdentity{Kind: "absent"}, false, material})
}

func newColdKeygenRequest(method, operation string, params any) (runtimeControlRequest, error) {
	id, err := controlUUID()
	if err != nil {
		return runtimeControlRequest{}, err
	}
	argument, err := json.Marshal(params)
	if err != nil {
		return runtimeControlRequest{}, err
	}
	defer clear(argument)
	return runtimeControlRequest{
		V: 1, ID: id, Method: method, Args: []string{string(argument)}, OperationID: operation, IdempotencyKey: operation,
	}, nil
}

func (m *Manager) probeColdKeygen(ctx context.Context, control runtimeAccountControl) error {
	req, err := newColdKeygenProbe(control.binding)
	if err != nil {
		return err
	}
	reply, err := m.exchangeRuntimeControlMode(ctx, control.image, control.port, control.grant, req, runtimeControlColdKeygen)
	defer clear(reply.Envelope)
	if err != nil || !isColdKeygenCapability(reply.Envelope) {
		return errors.New("Selected Runtime does not confirm cold keygen support")
	}
	return nil
}

func (m *Manager) exchangeColdKeygen(ctx context.Context, control runtimeAccountControl, op string, resultOnly bool, material runtimeRegisterMaterial) (string, error) {
	req, err := newColdKeygenOperation(control.binding, op, resultOnly, material)
	if err != nil {
		return "", err
	}
	reply, err := m.exchangeRuntimeControlMode(ctx, control.image, control.port, control.grant, req, runtimeControlColdKeygen)
	defer clear(reply.Envelope)
	if err == nil {
		if digest, ok := coldKeygenCommittedDigest(reply.Envelope, control.binding.Profile, op); ok {
			return digest, nil
		}
	}
	// Do not include private diagnostics or imply that a mutation may be retried.
	return "", &unconfirmedError{id: op, cause: errors.New("Runtime cold keygen receipt not confirmed")}
}

func isColdKeygenCapability(raw []byte) bool {
	summary, ok := coldKeygenSummary(raw, "keygen-capability", "protocol")
	return ok && coldKeygenString(summary, "protocol") == "cold_keygen.v1"
}

func coldKeygenCommittedDigest(raw []byte, profile, op string) (string, bool) {
	summary, ok := coldKeygenSummary(raw, "keygen", "operation_id", "outcome", "path", "session_sha256")
	if !ok || !filepath.IsAbs(profile) || !hostOperationID.MatchString(op) || coldKeygenString(summary, "operation_id") != op ||
		coldKeygenString(summary, "outcome") != "committed" || coldKeygenString(summary, "path") != filepath.Join(profile, "session.json") {
		return "", false
	}
	digest := coldKeygenString(summary, "session_sha256")
	return digest, validHash(digest)
}

func coldKeygenSummary(raw []byte, command string, required ...string) (map[string]json.RawMessage, bool) {
	envelope, ok := coldKeygenObject(raw, "command", "status", "summary", "items", "actions", "errors", "details", "related")
	if !ok || coldKeygenString(envelope, "command") != command || coldKeygenString(envelope, "status") != "ok" {
		return nil, false
	}
	if !coldKeygenReadyItems(envelope["items"]) {
		return nil, false
	}
	for _, key := range []string{"actions", "errors", "related"} {
		var entries []json.RawMessage
		if json.Unmarshal(envelope[key], &entries) != nil || entries == nil || len(entries) != 0 {
			return nil, false
		}
	}
	if !bytes.Equal(bytes.TrimSpace(envelope["details"]), []byte("null")) {
		return nil, false
	}
	return coldKeygenObject(envelope["summary"], required...)
}

func coldKeygenReadyItems(raw []byte) bool {
	var items []json.RawMessage
	if json.Unmarshal(raw, &items) != nil || len(items) != 2 {
		return false
	}
	for i, want := range [][3]string{{"control_ready", "ok", "true"}, {"store_ready", "skip", "false"}} {
		item, ok := coldKeygenObject(items[i], "name", "status", "value")
		if !ok || coldKeygenString(item, "name") != want[0] ||
			coldKeygenString(item, "status") != want[1] || coldKeygenString(item, "value") != want[2] {
			return false
		}
	}
	return true
}

func coldKeygenString(object map[string]json.RawMessage, key string) string {
	var result string
	if json.Unmarshal(object[key], &result) != nil {
		return ""
	}
	return result
}

// Strict exact fields and duplicate rejection at both envelope and summary.
func coldKeygenObject(raw []byte, required ...string) (map[string]json.RawMessage, bool) {
	decoder := json.NewDecoder(bytes.NewReader(raw))
	if token, err := decoder.Token(); err != nil || token != json.Delim('{') {
		return nil, false
	}
	out := make(map[string]json.RawMessage, len(required))
	for decoder.More() {
		token, err := decoder.Token()
		key, keyOK := token.(string)
		if err != nil || !keyOK || out[key] != nil || len(out) >= len(required) {
			return nil, false
		}
		var field json.RawMessage
		if decoder.Decode(&field) != nil {
			return nil, false
		}
		out[key] = field
	}
	if token, err := decoder.Token(); err != nil || token != json.Delim('}') {
		return nil, false
	}
	if _, err := decoder.Token(); err != io.EOF {
		return nil, false
	}
	for _, key := range required {
		if out[key] == nil {
			return nil, false
		}
	}
	return out, true
}
