//go:build linux || windows || (darwin && cgo)

package host

import (
	"encoding/json"
	"path/filepath"
	"strings"
	"testing"
)

const coldTestOperation = "4576ffb6-d245-4b73-86cf-59866145cb2a"

func TestColdKeygenPrivateTransport(t *testing.T) {
	root := t.TempDir()
	binding := runtimeControlBinding{InstallationRoot: root, Profile: root, LaunchID: coldTestOperation}
	probe, err := newColdKeygenProbe(binding)
	if err != nil {
		t.Fatal(err)
	}
	if probe.OperationID != "" || probe.IdempotencyKey != "" {
		t.Fatal("capability has operation identity")
	}
	if _, ok := coldKeygenObject([]byte(probe.Args[0]), "schema_version", "binding"); !ok {
		t.Fatal("capability contains unexpected parameters")
	}
	if flag, err := runtimeControlColdKeygen.flag(probe); err != nil || flag != "--host-cold-keygen-internal" {
		t.Fatal("probe did not select its fixed entry point")
	}
	if _, err := runtimeControlLegacy.flag(probe); err == nil {
		t.Fatal("capability must not fallback to legacy")
	}
	badProbe := probe
	badProbe.OperationID = coldTestOperation
	if _, err := runtimeControlColdKeygen.flag(badProbe); err == nil {
		t.Fatal("capability must not have operation identity")
	}
	badProbe = probe
	badProbe.Args = []string{`{"schema_version":1,"binding":{},"material":{}}`}
	if _, err := runtimeControlColdKeygen.flag(badProbe); err == nil {
		t.Fatal("capability must not carry material")
	}
	material := runtimeRegisterMaterial{Account: offlineKeygenAlias(coldTestOperation), Backend: runtimeLoginBackend(), Salt: "s", Wrapped: "w", Nonce: "n", Super: "synthetic"}
	for _, result := range []bool{false, true} {
		req, err := newColdKeygenOperation(binding, coldTestOperation, result, material)
		if err != nil {
			t.Fatal(err)
		}
		want := "control.auth.keygen"
		if result {
			want = "control.auth.result"
		}
		fields, ok := coldKeygenObject([]byte(req.Args[0]), "schema_version", "binding", "identity", "force", "material")
		if !ok || !reqIsColdKeygen(req, want) || string(fields["identity"]) != `{"kind":"absent"}` || string(fields["force"]) != "false" {
			t.Fatal("operation framing is not the exact cold keygen shape")
		}
		if _, err := runtimeControlColdKeygen.flag(req); err != nil {
			t.Fatal(err)
		}
		req.Method = "control.auth.login"
		if _, err := runtimeControlColdKeygen.flag(req); err == nil {
			t.Fatal("unrelated method entered cold launcher")
		}
	}
}

func reqIsColdKeygen(req runtimeControlRequest, want string) bool {
	return req.OperationID == coldTestOperation && req.IdempotencyKey == coldTestOperation && req.Method == want
}

func TestColdKeygenPrivateReceipts(t *testing.T) {
	env := func(command string, summary any) []byte {
		out, err := json.Marshal(map[string]any{"command": command, "status": "ok", "summary": summary,
			"items": []map[string]string{{"name": "control_ready", "status": "ok", "value": "true"},
				{"name": "store_ready", "status": "skip", "value": "false"}}, "actions": []any{}, "errors": []any{}, "details": nil, "related": []any{}})
		if err != nil {
			t.Fatal(err)
		}
		return out
	}
	probe := env("keygen-capability", map[string]string{"protocol": "cold_keygen.v1"})
	if !isColdKeygenCapability(probe) ||
		isColdKeygenCapability(env("keygen-capability", map[string]string{"protocol": "cold_keygen.v2"})) ||
		isColdKeygenCapability([]byte(strings.Replace(string(probe), `"protocol":`, `"protocol":"unknown","protocol":`, 1))) {
		t.Fatal("capability validation did not discriminate exact supported response")
	}
	for _, badItems := range []string{
		`[]`,
		`[{"name":"control_ready","status":"ok","value":"true","action":"unexpected"},{"name":"store_ready","status":"skip","value":"false"}]`,
		`[{"name":"control_ready","status":"ok","value":"true"},{"name":"store_ready","status":"ok","value":"true"}]`,
	} {
		var envelope map[string]json.RawMessage
		if err := json.Unmarshal(probe, &envelope); err != nil {
			t.Fatal(err)
		}
		envelope["items"] = json.RawMessage(badItems)
		raw, err := json.Marshal(envelope)
		if err != nil || isColdKeygenCapability(raw) {
			t.Fatal("accepted incorrect helper readiness")
		}
	}
	root := t.TempDir()
	digest := strings.Repeat("a", 64)
	summary := map[string]any{"operation_id": coldTestOperation, "outcome": "committed", "path": filepath.Join(root, "session.json"), "session_sha256": digest}
	if got, ok := coldKeygenCommittedDigest(env("keygen", summary), root, coldTestOperation); !ok || got != digest {
		t.Fatal("valid receipt rejected")
	}
	for field, value := range map[string]any{"path": "untrusted", "session_sha256": "bad", "outcome": "rolled_back", "operation_id": "other", "super_password": nil} {
		previous, had := summary[field]
		summary[field] = value
		if _, ok := coldKeygenCommittedDigest(env("keygen", summary), root, coldTestOperation); ok {
			t.Fatalf("accepted noncanonical receipt field %s", field)
		}
		if had {
			summary[field] = previous
		} else {
			delete(summary, field)
		}
	}
}
