package host

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"runtime"
	"time"
)

const ModelProbeTimeout = 180 * time.Second

var ErrInvalidModelVariant = errors.New("invalid model selection")
var ErrModelFilesMissing = errors.New("model files are missing or incomplete")

type modelVariant struct {
	Model   string `json:"model"`
	Variant string `json:"variant"`
}

func selectedModelVariant(model, variant string) (modelDefinition, string, []modelFileSpec, error) {
	definition, err := modelDefinitionByID(model)
	if err != nil {
		return definition, variant, nil, fmt.Errorf("%w: %w", ErrInvalidModelVariant, err)
	}
	variant = definition.selectedVariant(variant)
	files, err := definition.weightFiles(variant)
	if err != nil {
		return definition, variant, nil, fmt.Errorf("%w: %w", ErrInvalidModelVariant, err)
	}
	return definition, variant, files, nil
}

func validateModelEngine(engine string) error {
	switch engine {
	case "cpu", "gpu", "npu":
		return nil
	default:
		return fmt.Errorf("%w: engine must be cpu, gpu, or npu", ErrInvalidModelVariant)
	}
}

type ModelProbeResult struct {
	OK            bool            `json:"ok"`
	Model         string          `json:"model"`
	Variant       string          `json:"variant"`
	Engine        string          `json:"engine"`
	ClientVersion string          `json:"client_version"`
	OS            string          `json:"os"`
	Arch          string          `json:"arch"`
	Diagnostics   json.RawMessage `json:"diagnostics,omitempty"`
	Runtime       json.RawMessage `json:"runtime"`
}

func (m *Manager) ProbeModelVariant(ctx context.Context, model, variant, engine string) (ModelProbeResult, error) {
	result := ModelProbeResult{
		Model: model, Variant: variant, Engine: engine,
		ClientVersion: m.ClientVersion, OS: runtime.GOOS, Arch: runtime.GOARCH,
	}
	if err := validateModelEngine(engine); err != nil {
		return result, err
	}
	definition, variant, files, err := selectedModelVariant(model, variant)
	if err != nil {
		return result, err
	}
	result.Variant = variant
	release, err := m.holdProfile()
	if err != nil {
		return result, err
	}
	defer release()
	dir := m.ProfileDir(model)
	if definition.Legacy {
		dir, err = m.m3VariantDir(ctx, files)
		if err != nil {
			return result, err
		}
	}
	weights := filepath.Join(dir, files[0].rel)
	tokenizer := filepath.Join(dir, definition.Tokenizer.rel)
	if !modelWeightsPresent(dir, append([]modelFileSpec{definition.Tokenizer}, files...)) {
		if !definition.Legacy || variant != "quantized" {
			return result, ErrModelFilesMissing
		}
		health, supported, err := m.runtimeModelInstallCapability(ctx)
		if err != nil {
			return result, err
		}
		managedWeights, managedTokenizer, installed := m.managedM3Pair(health)
		if !supported || !installed {
			return result, ErrModelFilesMissing
		}
		weights, tokenizer = managedWeights, managedTokenizer
	}
	companions := make([]string, 0, len(files)-1)
	for _, file := range files[1:] {
		companions = append(companions, filepath.Join(dir, file.rel))
	}
	encoded, err := json.Marshal(map[string]any{
		"model": model, "variant": variant, "engine": engine,
		"weights_path": weights, "tokenizer_path": tokenizer, "companion_paths": companions,
		"dimensions": definition.Dimensions, "max_tokens": definition.MaxTokens,
		"query_prefix": definition.QueryPrefix, "pad_token_id": definition.PadTokenID, "granite": definition.Granite,
	})
	if err != nil {
		return result, err
	}
	result.Runtime, err = m.modelRPCReply(ctx, "model.probe", []string{string(encoded)})
	if err != nil {
		return result, err
	}
	var reply struct {
		OK       *bool `json:"ok"`
		Exit     int   `json:"exit"`
		Envelope struct {
			Status  string          `json:"status"`
			Summary json.RawMessage `json:"summary"`
		} `json:"envelope"`
	}
	if err := json.Unmarshal(result.Runtime, &reply); err != nil {
		return result, err
	}
	if reply.OK == nil {
		return result, errors.New("Runtime model probe reply has no status")
	}
	var diagnostics struct {
		OK *bool `json:"ok"`
	}
	result.Diagnostics = reply.Envelope.Summary
	if len(result.Diagnostics) > 0 {
		if err := json.Unmarshal(result.Diagnostics, &diagnostics); err != nil {
			return result, err
		}
	}
	result.OK = *reply.OK && reply.Exit == 0 && reply.Envelope.Status != "fail" &&
		(diagnostics.OK == nil || *diagnostics.OK)
	return result, nil
}
