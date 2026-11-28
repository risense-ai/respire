package host

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
)

var ErrModelVariantBusy = errors.New("model variant is active or has a model operation in progress")

type runtimeModelSelection struct {
	modelVariant
	Profile struct {
		Weights struct {
			Path string `json:"path"`
		} `json:"weights"`
	} `json:"profile"`
}

func (s runtimeModelSelection) references(key modelVariant, weights string) bool {
	if s.Profile.Weights.Path != "" && samePath(s.Profile.Weights.Path, weights) {
		return true
	}
	if s.Model == "bge-m3" {
		s.Model = "m3"
	}
	if s.Model != key.Model {
		return false
	}
	if s.Variant == "" {
		if s.Profile.Weights.Path != "" {
			return filepath.Base(s.Profile.Weights.Path) == filepath.Base(weights)
		}
		return true
	}
	definition, _ := modelDefinitionByID(key.Model)
	return definition.selectedVariant(s.Variant) == key.Variant
}

func modelEnvelopeField(raw json.RawMessage, key string) (json.RawMessage, error) {
	var envelope struct {
		Summary map[string]json.RawMessage `json:"summary"`
		Details map[string]json.RawMessage `json:"details"`
	}
	if err := json.Unmarshal(raw, &envelope); err != nil {
		return nil, err
	}
	if value, ok := envelope.Details[key]; ok {
		return value, nil
	}
	return envelope.Summary[key], nil
}

func (m *Manager) checkModelDeletion(ctx context.Context, key modelVariant, weights string) error {
	progress, err := m.ModelProgress(ctx)
	if err != nil {
		return err
	}
	var pending *runtimeModelSelection
	raw, err := modelEnvelopeField(progress, "pending_selection")
	if err != nil {
		return err
	}
	if raw != nil {
		if err := json.Unmarshal(raw, &pending); err != nil {
			return err
		}
	}
	if pending != nil && pending.references(key, weights) {
		return ErrModelVariantBusy
	}
	var switching struct {
		State  string `json:"state"`
		Target string `json:"target"`
	}
	var operation struct {
		Active bool `json:"active"`
	}
	for field, target := range map[string]any{"switch": &switching, "operation": &operation} {
		raw, err := modelEnvelopeField(progress, field)
		if err != nil {
			return err
		}
		if len(raw) == 0 {
			return fmt.Errorf("Runtime model progress has no %s field", field)
		}
		if err := json.Unmarshal(raw, target); err != nil {
			return err
		}
	}
	if (switching.State == "pending" &&
		(switching.Target == "" || switching.Target == key.Model || switching.Target == "bge-m3" && key.Model == "m3")) ||
		(operation.Active && switching.State != "pending") {
		return ErrModelVariantBusy
	}
	catalog, err := m.runtimeRPC(ctx, "model.catalog", nil)
	if err != nil {
		return err
	}
	raw, err = modelEnvelopeField(catalog, "active")
	if err != nil {
		return err
	}
	if len(raw) == 0 {
		return errors.New("Runtime model catalog has no active selection")
	}
	var active *runtimeModelSelection
	if err := json.Unmarshal(raw, &active); err != nil {
		return err
	}
	if active == nil {
		if key.Model == "m3" && key.Variant != "bnb4" && key.Variant != "fp32" {
			return m.checkLegacyM3Deletion(ctx, key, weights)
		}
		return nil
	}
	if active.references(key, weights) {
		return ErrModelVariantBusy
	}
	return nil
}

func (m *Manager) checkLegacyM3Deletion(ctx context.Context, key modelVariant, weights string) error {
	raw, err := m.runtimeRPC(ctx, "assets.status", nil)
	if err != nil {
		return err
	}
	assets, err := modelEnvelopeField(raw, "assets")
	if err != nil {
		return err
	}
	var required []struct {
		Name  string `json:"name"`
		Path  string `json:"path"`
		State string `json:"state"`
	}
	if err := json.Unmarshal(assets, &required); err != nil {
		return err
	}
	for _, asset := range required {
		if asset.Path != "" && (samePath(asset.Path, weights) || key.Variant == "quantized" &&
			(asset.Name == "bge-m3-cpu-ort" || asset.Name == "bge-m3-model" && asset.State == "")) {
			return ErrModelVariantBusy
		}
	}
	for _, asset := range required {
		if (asset.Name == "bge-m3-model" || asset.Name == "bge-m3-cpu-ort") && asset.Path != "" {
			return nil
		}
	}
	return errors.New("Runtime did not identify legacy M3 assets")
}

func (m *Manager) DeleteModelVariant(ctx context.Context, model, variant string) (string, error) {
	definition, variant, files, err := selectedModelVariant(model, variant)
	if err != nil {
		return "", err
	}
	key := modelVariant{Model: model, Variant: variant}
	m.modelMu.Lock()
	defer m.modelMu.Unlock()
	if m.modelDownloads[key] {
		return "", ErrModelVariantBusy
	}
	release, err := m.holdProfile()
	if err != nil {
		return "", err
	}
	defer release()
	dir := m.ProfileDir(model)
	if definition.Legacy {
		dir, err = m.m3VariantDir(ctx, files)
		if err != nil {
			return "", err
		}
	}
	if err := m.checkModelDeletion(ctx, key, filepath.Join(dir, files[0].rel)); err != nil {
		return "", err
	}
	if err := checkModelUninstallPath(dir); err != nil {
		return "", err
	}
	if err := checkModelUninstallPath(filepath.Join(dir, "onnx")); err != nil {
		return "", err
	}
	root, err := os.OpenRoot(dir)
	if errors.Is(err, os.ErrNotExist) {
		return variant, nil
	}
	if err != nil {
		return "", err
	}
	defer root.Close()
	for _, file := range files {
		info, err := root.Lstat(file.rel)
		if errors.Is(err, os.ErrNotExist) {
			continue
		}
		if err != nil {
			return "", err
		}
		if !info.Mode().IsRegular() || modelUninstallRedirect(info) {
			return "", fmt.Errorf("model weight is not a regular file: %s", file.rel)
		}
	}
	if err := ctx.Err(); err != nil {
		return "", err
	}
	for _, file := range files {
		if err := root.Remove(file.rel); err != nil && !errors.Is(err, os.ErrNotExist) {
			return "", err
		}
	}
	return variant, nil
}
