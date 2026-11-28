package host

import (
	"context"
	"crypto/rand"
	"encoding/json"
	"errors"

	runtimesdk "github.com/risense-ai/respire/packages/runtime-sdk-go"
)

const modelSwitchRPCPrefix = "client2-model-switch-"

func (m *Manager) startModelSwitchRPC(ctx context.Context, model, engine, variant string) (ModelSwitchReceipt, error) {
	selection, err := json.Marshal(map[string]string{
		"model": model, "backend": "onnx", "engine": engine, "variant": variant,
	})
	if err != nil {
		return ModelSwitchReceipt{}, err
	}
	id := modelSwitchRPCPrefix + rand.Text()
	raw, err := m.executeRPC(ctx, "model.switch", []string{string(selection)}, id)
	var pending *unconfirmedError
	if err != nil && (!errors.As(err, &pending) || len(raw) == 0) {
		return ModelSwitchReceipt{}, err
	}
	return ModelSwitchReceipt{OperationAccepted: &runtimesdk.OperationAccepted{
		OperationId: id, Status: "accepted",
	}, Legacy: true}, nil
}

func (m *Manager) modelSwitchRPCStatus(ctx context.Context, id string, cancel bool) (json.RawMessage, error) {
	if cancel {
		raw, err := m.runtimeRPC(ctx, "model.control", []string{id, "true"})
		if err != nil {
			return nil, err
		}
		summary, err := modelEnvelopeField(raw, "cancelled")
		if err != nil {
			return nil, err
		}
		if string(summary) != "true" {
			return nil, errors.New("Runtime did not accept model switch cancellation")
		}
	}
	raw, err := m.runtimeRPC(ctx, "runtime.result", []string{id})
	if err != nil {
		return nil, err
	}
	var receipt struct {
		Summary struct {
			OperationID string       `json:"operation_id"`
			State       string       `json:"state"`
			Response    *rpcResponse `json:"response"`
		} `json:"summary"`
	}
	if err := json.Unmarshal(raw, &receipt); err != nil {
		return nil, err
	}
	summary := receipt.Summary
	if summary.OperationID != id {
		return nil, errors.New("Runtime switch status has an unexpected operation ID")
	}
	status := map[string]any{"operation_id": id, "kind": "model_switch", "state": summary.State}
	switch summary.State {
	case "pending":
	case "completed":
		if summary.Response == nil || !summary.Response.OK {
			return nil, errors.New("Runtime switch has no completion receipt")
		}
	case "failed":
		if summary.Response != nil {
			status["message"] = summary.Response.Error
		}
	default:
		return nil, errors.New("Runtime could not confirm the model switch status")
	}
	return json.Marshal(status)
}
