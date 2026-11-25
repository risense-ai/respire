package host

import (
	"encoding/json"
	"errors"
	"testing"
)

func TestCompletedOrganizePreviewPreservesBoundReadonlyReceipt(t *testing.T) {
	const id = "desktop-00000000-0000-4000-8000-000000000001"
	scope := runtimeScope{"synthetic-account", "synthetic-runtime", 4}
	args := []string{"--json", "resort", "--spec={\"ops\":[{\"id\":\"test-child\",\"parent\":\"test-parent\"}]}"}
	command := "cli.exec"
	valid := json.RawMessage("{\"command\":\"resort\",\"status\":\"pending\",\"summary\":{\"moved\":1,\"applied\":false},\"items\":[{\"name\":\"test-chi\",\"status\":\"pending\",\"value\":\"-> test-par fixture\"}]}")
	reply := rpcResponse{V: 1, ID: id, Exit: 2, OperationID: id, Scope: &scope, Envelope: valid}
	if !completedOrganizePreview(id, &scope, command, args, reply) {
		t.Fatal("matching read-only dry-run receipt was rejected")
	}
	var pending *unconfirmedError
	if !errors.As(responseError(reply), &pending) {
		t.Fatal("generic pending handling must remain unchanged")
	}
	for _, tc := range []struct {
		name   string
		mutate func(*rpcResponse)
	}{
		{"different request", func(r *rpcResponse) { r.ID = "other" }},
		{"different operation", func(r *rpcResponse) { r.OperationID = "other" }},
		{"different scope", func(r *rpcResponse) { r.Scope = &runtimeScope{"synthetic-account", "synthetic-runtime", 5} }},
		{"missing scope", func(r *rpcResponse) { r.Scope = nil }},
		{"error code", func(r *rpcResponse) { r.Code = "failed" }},
		{"error text", func(r *rpcResponse) { r.Error = "failed" }},
		{"wrong exit", func(r *rpcResponse) { r.Exit = 1 }},
		{"unexpected command", func(r *rpcResponse) {
			r.Envelope = json.RawMessage("{\"command\":\"model.switch\",\"status\":\"pending\"}")
		}},
		{"missing applied flag", func(r *rpcResponse) {
			r.Envelope = json.RawMessage("{\"command\":\"resort\",\"status\":\"pending\",\"summary\":{\"moved\":1},\"items\":[{\"name\":\"test-chi\",\"status\":\"pending\",\"value\":\"test\"}]}")
		}},
		{"applied", func(r *rpcResponse) {
			r.Envelope = json.RawMessage("{\"command\":\"resort\",\"status\":\"pending\",\"summary\":{\"moved\":1,\"applied\":true},\"items\":[{\"name\":\"test-chi\",\"status\":\"pending\",\"value\":\"test\"}]}")
		}},
		{"typed shape", func(r *rpcResponse) {
			r.Envelope = json.RawMessage("{\"command\":\"resort\",\"status\":\"pending\",\"summary\":{\"moved\":1,\"applied\":false},\"items\":[{\"name\":\"test-chi\",\"status\":\"pending\",\"value\":{}}]}")
		}},
		{"missing output row", func(r *rpcResponse) {
			r.Envelope = json.RawMessage("{\"command\":\"resort\",\"status\":\"pending\",\"summary\":{\"moved\":1,\"applied\":false},\"items\":[]}")
		}},
	} {
		t.Run(tc.name, func(t *testing.T) {
			altered := reply
			tc.mutate(&altered)
			if completedOrganizePreview(id, &scope, command, args, altered) {
				t.Fatal("unsafe or unconfirmed receipt was accepted")
			}
		})
	}
	if completedOrganizePreview(id, &scope, command, append(append([]string{}, args...), "--go"), reply) ||
		completedOrganizePreview(id, &scope, "model.switch", args, reply) {
		t.Fatal("a different or mutating operation accepted preview semantics")
	}
}
