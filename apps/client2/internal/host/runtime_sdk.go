package host

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"slices"
	"strings"

	runtimesdk "github.com/risense-ai/respire/packages/runtime-sdk-go"
)

type RuntimeHTTPError struct {
	Status int
	Code   string
	Detail string
}

func (e *RuntimeHTTPError) Error() string {
	return fmt.Sprintf("Runtime HTTP %d [%s]: %s", e.Status, e.Code, e.Detail)
}

func (m *Manager) RuntimeSupports(ctx context.Context, capability string) (bool, error) {
	_, supported, err := m.runtimeSDK(ctx, capability)
	return supported, err
}

func (m *Manager) runtimeSDK(ctx context.Context, capability string) (*runtimesdk.Client, bool, error) {
	client, err := runtimesdk.NewClient(m.Endpoint, runtimesdk.WithHTTPClient(m.localClient))
	if err != nil {
		return nil, false, err
	}
	health, err := discoverRuntime(ctx, client)
	if err != nil || health == nil {
		return client, false, err
	}
	return client, slices.Contains(health.Capabilities, capability), nil
}

// Read existing installation credentials; discovery must never create credentials.
func (m *Manager) authorizeRuntime(ctx context.Context, req *http.Request) error {
	token := strings.TrimSpace(os.Getenv("RSRS_RPC_TOKEN"))
	if token == "" {
		raw, err := readCredentialConfig(filepath.Join(m.DataRoot, "runtime", "token"), 65536)
		if err != nil {
			return err
		}
		token = strings.TrimSpace(string(raw))
	}
	if token == "" || strings.ContainsAny(token, "\x00\r\n") {
		return errors.New("Runtime authentication token is invalid")
	}
	req.Header.Set("Authorization", "Bearer "+token)
	return nil
}

func sdkJSON(response *http.Response, err error) (json.RawMessage, error) {
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	if response.StatusCode < 200 || response.StatusCode >= 300 {
		var problem runtimesdk.ProblemDetails
		if decodeErr := json.NewDecoder(response.Body).Decode(&problem); decodeErr != nil {
			return nil, &RuntimeHTTPError{Status: response.StatusCode, Detail: decodeErr.Error()}
		}
		return nil, &RuntimeHTTPError{Status: response.StatusCode, Code: problem.Code, Detail: problem.Title + ": " + problem.Detail}
	}
	if response.StatusCode == http.StatusNoContent {
		return json.RawMessage("{}"), nil
	}
	var raw json.RawMessage
	if err := json.NewDecoder(response.Body).Decode(&raw); err != nil {
		return nil, err
	}
	return raw, nil
}

func desktopEnvelope(command string, summary any) (json.RawMessage, error) {
	return json.Marshal(map[string]any{"command": command, "status": "ok", "summary": summary})
}

type ModelSwitchReceipt struct {
	*runtimesdk.OperationAccepted
	Stream bool `json:"stream"`
	Legacy bool `json:"legacy,omitempty"`
}

func (m *Manager) StartModelSwitch(ctx context.Context, model, engine, variant string) (ModelSwitchReceipt, error) {
	client, supported, err := m.runtimeSDK(ctx, "openapi.v1")
	if err != nil {
		return ModelSwitchReceipt{}, err
	}
	if !supported || engine != "cpu" {
		if model == "m3" {
			return ModelSwitchReceipt{Legacy: true}, nil
		}
		if err := m.ModelConfigure(ctx, model, variant); err != nil {
			return ModelSwitchReceipt{}, err
		}
		return ModelSwitchReceipt{}, m.ModelSwitch(ctx, model, engine, variant)
	}
	definition, err := modelDefinitionByID(model)
	if err != nil {
		return ModelSwitchReceipt{}, err
	}
	if !definition.Legacy {
		if err := m.ModelConfigure(ctx, model, variant); err != nil {
			return ModelSwitchReceipt{}, err
		}
	}
	var selectedVariant *string
	if definition.GraniteWeightsPath == "" {
		selectedVariant = &variant
	}
	_, stream, err := m.runtimeSDK(ctx, "sse.operations.v1")
	if err != nil {
		return ModelSwitchReceipt{}, err
	}
	raw, err := sdkJSON(client.SwitchModel(ctx, runtimesdk.SwitchModelRequest{
		ModelId: model, Engine: &engine, Variant: selectedVariant,
	}, m.authorizeRuntime))
	if err != nil {
		return ModelSwitchReceipt{}, err
	}
	var accepted runtimesdk.OperationAccepted
	if err := json.Unmarshal(raw, &accepted); err != nil {
		return ModelSwitchReceipt{}, err
	}
	if accepted.OperationId == "" {
		return ModelSwitchReceipt{}, errors.New("Runtime switch receipt has no operation ID")
	}
	return ModelSwitchReceipt{OperationAccepted: &accepted, Stream: stream}, err
}

func (m *Manager) RuntimeOperation(ctx context.Context, id string, cancel bool) (json.RawMessage, error) {
	capability := "openapi.v1"
	if cancel {
		capability = "sse.operations.v1"
	}
	client, supported, err := m.runtimeSDK(ctx, capability)
	if err != nil {
		return nil, err
	}
	if !supported {
		return nil, fmt.Errorf("Runtime does not advertise %s", capability)
	}
	if cancel {
		return sdkJSON(client.CancelOperation(ctx, id, m.authorizeRuntime))
	}
	return sdkJSON(client.GetOperation(ctx, id, m.authorizeRuntime))
}
