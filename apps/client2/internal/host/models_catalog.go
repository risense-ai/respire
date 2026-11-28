package host

import "respire-client2/internal/i18n"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"path/filepath"
	"time"
)

type modelDefinition struct {
	ID          string
	Label       string
	Repo        string
	Revision    string
	Dimensions  int
	MaxTokens   int
	QueryPrefix string
	Granite     bool
	PadTokenID  int
	Tokenizer   modelFileSpec
	// Variants in display order, with the default first.
	Variants []modelVariantFile
	// Hashes keyed by variant name.
	Hashes map[string]string
	// M3 keeps its historical installation layout.
	Legacy bool
}

const DefaultModelID = "granite-311m-r2"

var modelCatalog = []modelDefinition{
	{
		ID: DefaultModelID, Label: i18n.Messagef3f81b431585bcc7,
		Repo: "ibm-granite/granite-embedding-311m-multilingual-r2", Revision: "44399559930365213510b1ee2eb15ded83374f0e",
		Dimensions: 768, MaxTokens: 32768, Granite: true,
		Tokenizer: modelFileSpec{rel: "tokenizer.json", sha256: "0087c868b33bad550a78a08d19798cfd7f713cde4f020803b8f51f405503e15f", size: 33384821},
		Variants:  []modelVariantFile{{"quint8_avx2", 313421909}, {"standard", 1247170481}},
		Hashes: map[string]string{
			"quint8_avx2": "f1fdd44e7e1ac51f12ab7957c7bd092e064d596c288513bf9d326842f669edee",
			"standard":    "75f9f258bf5013f5fe8a4dad61dd0fd16ac0cbaa7a106e3d3f41c2d04a42d541",
		},
	},
	{
		ID: "granite-97m-r2", Label: i18n.Message34266d29aed50583,
		Repo: "ibm-granite/granite-embedding-97m-multilingual-r2", Revision: "835ad14087e140460703cf0fae09f97d469d65c2",
		Dimensions: 384, MaxTokens: 32768, Granite: true, PadTokenID: 179935,
		Tokenizer: modelFileSpec{rel: "tokenizer.json", sha256: "4f2842d568e2724370aec203652a42ac783c7937f8347a1a2cc7506d71f1582f", size: 25301672},
		Variants:  []modelVariantFile{{"quint8_avx2", 98247878}, {"standard", 390004608}},
		Hashes: map[string]string{
			"quint8_avx2": "a6022dd8220ea6f6595562a1328ee216f4a94faa55362f2f4747c80f1e78772e",
			"standard":    "68e592b160673d30250824c1116bc6ab33f70efb22b97c9e1d7ce1e69c1c9d70",
		},
	},
	{
		ID: "m3", Label: i18n.Messagea0e7ed5e45bbad7e,
		Repo: "Xenova/bge-m3", Revision: modelRevision,
		Dimensions: 1024, MaxTokens: 512, Legacy: true,
		Tokenizer: modelFileSpecs["tokenizer"],
		Variants:  modelVariantFiles,
	},
	{
		ID: "bge-base-zh-v1.5", Label: i18n.Messagedd25263c4916820a,
		Repo: "Xenova/bge-base-zh-v1.5", Revision: "71e50dc531959f9e04ebf190ea25b00261a0a186",
		Dimensions: 768, MaxTokens: 512,
		QueryPrefix: i18n.Message9ce5caa4213d4b09,
		Tokenizer:   modelFileSpec{rel: "tokenizer.json", sha256: "7dfbf1966ebf99d471c3796e9b457329d2b2182b817e144f1e904b957745c839", size: 439124},
		Variants: []modelVariantFile{
			{"quantized", 102868746},
			{"fp16", 203679052},
			{"int8", 102407793},
			{"uint8", 102407824},
			{"q4", 120342974},
			{"q4f16", 81549931},
			{"bnb4", 115035086},
			{"fp32", 406953171},
		},
		Hashes: map[string]string{
			"quantized": "b665f3bba56c3119bc76ba131ebcc544d720a7408cb11581bdf354aaa0198d43",
			"fp16":      "9ed3f10ed06aafe0320567019d18fdec255339350525a33ae18f85f4def288f0",
			"int8":      "c11bdcfad37eaf53e43362885cbe5fad1b44d5b704da753a04f8c0b2f7768064",
			"uint8":     "5e2f9e9f07472751e0a5a0b21bcfa49cb0ac1eaab094784a3f30425a8bcc1d80",
			"q4":        "dab1bb68aaf39bbfa4ef7d218091174f01cd25781a07f36c654a2e3037d4376a",
			"q4f16":     "8278a219a9ae056860dcde7ce8241bee057a6ab0243454c85facf4d6e8325777",
			"bnb4":      "85bb3a0990c1ebf3e575d7a43938da383ae7233eba27af7e19341553e4583594",
			"fp32":      "5e5619f7cca7380b824d329c157dba10bee7cc00d0c139e82fdb7906051b8e4f",
		},
	},
}

func modelDefinitionByID(id string) (modelDefinition, error) {
	for _, definition := range modelCatalog {
		if definition.ID == id {
			return definition, nil
		}
	}
	return modelDefinition{}, errors.New(i18n.Message5cfcb7bc92afad64 + id)
}

// ProfileDir is the on-disk weight directory for profile models; the runtime
// profile points at these files (model.configure resources).
func (m *Manager) ProfileDir(model string) string {
	return filepath.Join(m.DataRoot, "models", model)
}

func (definition modelDefinition) selectedVariant(variant string) string {
	if variant == "" || (definition.Granite && variant == "quantized") {
		return definition.Variants[0].Name
	}
	return variant
}

// modelFileSpecFor returns the download spec for one variant of a model.
func (definition modelDefinition) modelFileSpecFor(variant string) (modelFileSpec, error) {
	if definition.Legacy && variant != "tokenizer" {
		if spec, ok := modelFileSpecs[variant]; ok {
			return spec, nil
		}
	}
	hash, ok := definition.Hashes[variant]
	if !ok {
		return modelFileSpec{}, errors.New(i18n.Messagef81dd9a5fc1b2073 + variant)
	}
	for _, candidate := range definition.Variants {
		if candidate.Name == variant {
			return modelFileSpec{rel: variantFileName(variant), sha256: hash, size: candidate.Size}, nil
		}
	}
	return modelFileSpec{}, errors.New(i18n.Messagef81dd9a5fc1b2073 + variant)
}

func (definition modelDefinition) weightFiles(variant string) ([]modelFileSpec, error) {
	weights, err := definition.modelFileSpecFor(variant)
	if err != nil {
		return nil, err
	}
	files := []modelFileSpec{weights}
	if definition.Legacy && variant == "fp32" {
		files = append(files, m3ExternalData)
	}
	return files, nil
}

// ConfigurePayload builds the model.configure argument for a profile model.
func (definition modelDefinition) ConfigurePayload(variant, weightsPath string) (string, error) {
	files, err := definition.weightFiles(variant)
	if err != nil {
		return "", err
	}
	weights := files[0]
	onnx := map[string]string{
		"input_ids": "input_ids", "attention_mask": "attention_mask", "output": "last_hidden_state",
	}
	if !definition.Granite {
		onnx["token_type_ids"] = "token_type_ids"
	}
	profile := map[string]any{
		"id": definition.ID,
		"embedding": map[string]any{
			"model_id":          definition.Repo,
			"revision":          definition.Revision,
			"dimensions":        definition.Dimensions,
			"max_tokens":        definition.MaxTokens,
			"pooling":           "cls",
			"normalize":         true,
			"query_prefix":      definition.QueryPrefix,
			"document_prefix":   "",
			"pad_token_id":      definition.PadTokenID,
			"pad_to_max_tokens": false,
		},
		"onnx": onnx,
		"weights": map[string]any{
			"path": weightsPath, "size": weights.size, "sha256": weights.sha256,
		},
		"tokenizer": map[string]any{
			"path": filepath.Join(filepath.Dir(weightsPath), "..", "tokenizer.json"), "size": definition.Tokenizer.size, "sha256": definition.Tokenizer.sha256,
		},
	}
	if len(files) > 1 {
		companion := files[1]
		profile["companions"] = []map[string]any{{
			"path": filepath.Join(filepath.Dir(weightsPath), filepath.Base(companion.rel)),
			"size": companion.size, "sha256": companion.sha256,
		}}
	}
	encoded, err := json.Marshal(map[string]any{"model_profile": profile})
	if err != nil {
		return "", err
	}
	return string(encoded), nil
}

// runtimeRPC calls the resident runtime loopback /api/rpc with the v:1
// envelope the host RPC documents (model.catalog / configure / switch / ...).
func (m *Manager) runtimeRPC(ctx context.Context, method string, args []string) (json.RawMessage, error) {
	raw, err := m.modelRPCReply(ctx, method, args)
	if err != nil {
		return nil, err
	}
	var reply struct {
		OK       bool            `json:"ok"`
		Envelope json.RawMessage `json:"envelope"`
		Error    string          `json:"error"`
		Errors   []string        `json:"errors"`
	}
	if err := json.Unmarshal(raw, &reply); err != nil {
		return nil, fmt.Errorf(i18n.Message1159c2b08a1760fe, err)
	}
	if !reply.OK {
		message := i18n.Messagedb4136989bd8b201
		if reply.Error != "" {
			message = reply.Error
		} else if len(reply.Errors) > 0 {
			message = reply.Errors[0]
		}
		return nil, errors.New(message)
	}
	if len(reply.Envelope) == 0 {
		return json.RawMessage("{}"), nil
	}
	var envelope struct {
		Status string   `json:"status"`
		Errors []string `json:"errors"`
	}
	_ = json.Unmarshal(reply.Envelope, &envelope)
	if envelope.Status == "fail" {
		message := i18n.Messagee4501161a6d3b24c
		if len(envelope.Errors) > 0 {
			message = envelope.Errors[0]
		}
		return nil, errors.New(message)
	}
	return reply.Envelope, nil
}

func (m *Manager) modelRPCReply(ctx context.Context, method string, args []string) (json.RawMessage, error) {
	if args == nil {
		args = []string{}
	}
	return m.legacyRPCRequest(ctx, map[string]any{
		"v":      1,
		"id":     fmt.Sprintf("client2-model-%d", time.Now().UnixNano()),
		"method": method,
		"args":   args,
	})
}

// ModelCatalogRPC reports the registry plus the active selection and any
// pending switch task.
func (m *Manager) ModelCatalogRPC(ctx context.Context) (json.RawMessage, error) {
	raw, handled, err := m.modelCatalogV1(ctx)
	if err == nil && !handled {
		raw, err = m.runtimeRPC(ctx, "model.catalog", nil)
	}
	if err != nil {
		return nil, err
	}
	var envelope map[string]any
	if err := json.Unmarshal(raw, &envelope); err != nil {
		return nil, err
	}
	models := make([]map[string]string, 0, len(modelCatalog))
	for _, definition := range modelCatalog {
		models = append(models, map[string]string{
			"id": definition.ID, "label": definition.Label, "default_variant": definition.Variants[0].Name,
		})
	}
	envelope["selectable_models"] = models
	envelope["default_model"] = DefaultModelID
	return json.Marshal(envelope)
}

// ModelConfigure registers a profile model's resources with the runtime.
func (m *Manager) ModelConfigure(ctx context.Context, model, variant string) error {
	m.modelMu.Lock()
	defer m.modelMu.Unlock()
	return m.configureModel(ctx, model, variant)
}

func (m *Manager) configureModel(ctx context.Context, model, variant string) error {
	definition, err := modelDefinitionByID(model)
	if err != nil {
		return err
	}
	variant = definition.selectedVariant(variant)
	files, err := definition.weightFiles(variant)
	if err != nil {
		return err
	}
	dir := m.ProfileDir(model)
	if definition.Legacy {
		dir, err = m.m3VariantDir(ctx, files)
		if err != nil {
			return err
		}
	}
	payload, err := definition.ConfigurePayload(variant, filepath.Join(dir, files[0].rel))
	if err != nil {
		return err
	}
	_, err = m.runtimeRPC(ctx, "model.configure", []string{payload})
	return err
}

// ModelProgress reports the pending switch task and active selection.
func (m *Manager) ModelProgress(ctx context.Context) (json.RawMessage, error) {
	return m.runtimeRPC(ctx, "model.progress", nil)
}
