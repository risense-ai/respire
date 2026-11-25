package host

import (
	"encoding/json"
	"strings"
)

// The legacy CLI reports a completed, read-only resort preview as Pending
// (exit 2) because its plan has not been applied. That is not an unfinished
// operation. Do not normalize any other pending or mutating command.
func completedOrganizePreview(id string, scope *runtimeScope, method string, args []string, reply rpcResponse) bool {
	if method != "cli.exec" || len(args) != 3 || args[0] != "--json" || args[1] != "resort" ||
		scope == nil || scope.AccountID == "" || scope.InstanceID == "" || scope.Generation == 0 ||
		reply.V != 1 || reply.ID != id || reply.OperationID != id || reply.Scope == nil || *reply.Scope != *scope ||
		reply.OK || reply.Exit != 2 || reply.Code != "" || reply.Error != "" {
		return false
	}
	spec, hasSpec := strings.CutPrefix(args[2], "--spec=")
	var submitted struct {
		Ops []struct {
			ID     string
			Parent string
		}
	}
	if !hasSpec || json.Unmarshal([]byte(spec), &submitted) != nil || len(submitted.Ops) == 0 {
		return false
	}
	var preview struct {
		Command string
		Status  string
		Summary struct {
			Moved   int
			Applied *bool
		}
		Items []struct {
			Name   string
			Status string
			Value  string
		}
		Errors []string
	}
	if json.Unmarshal(reply.Envelope, &preview) != nil || preview.Command != "resort" ||
		preview.Status != "pending" || preview.Summary.Applied == nil || *preview.Summary.Applied ||
		preview.Summary.Moved != len(submitted.Ops) || len(preview.Items) != len(submitted.Ops) ||
		len(preview.Errors) != 0 {
		return false
	}
	for index, item := range preview.Items {
		op := submitted.Ops[index]
		if op.ID == "" || op.Parent == "" || item.Name == "" || item.Value == "" || item.Status != "pending" {
			return false
		}
	}
	return true
}
