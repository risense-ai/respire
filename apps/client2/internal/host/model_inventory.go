package host

import (
	"context"
	"encoding/json"
	"os"
	"path/filepath"
	"strings"

	"respire-client2/internal/i18n"
)

// BGE-M3 onnx variants accepted by the runtime's `model engine --model` and
// `model install-m3 --model` (cli MODEL_VARIANTS). Sizes must mirror
// crates/core-sdk/src/business.rs so install state can be verified locally
// without hashing the multi-hundred-MiB files on every poll.
type modelVariantFile struct {
	Name string
	Size int64
}

var modelVariantFiles = []modelVariantFile{
	{"quantized", 569694530},
	{"fp16", 1133992936},
	{"int8", 568456694},
	{"uint8", 568456765},
	{"q4", 1248237611},
	{"q4f16", 699932840},
}

// modelVariantByName returns the pinned file name relative to the model dir.
func variantFileName(name string) string {
	return filepath.Join("onnx", "model_"+name+".onnx")
}

// modelDir resolves the runtime's BGE-M3 directory the same way the CLI's
// resolve_model_dir does: <main data dir>/models/bge-m3 first — the
// RSRS_DATA_DIR root or ~/.rsrs, reported by health as installation_root —
// then the legacy profile dirs (only for runtimes launched without
// RSRS_DATA_DIR) and the packaged copy next to the runtime executable.
// health.data_dir is the workspace profile (client.json data_dir) and must
// not be used for model files.
func (m *Manager) modelDir(ctx context.Context) (string, error) {
	health, _, err := m.Health(ctx)
	if err != nil {
		return "", err
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	if health.InstallationRoot == "" {
		health.InstallationRoot = filepath.Join(home, ".rsrs")
	}
	// A managed runtime is launched with RSRS_DATA_DIR=installation_root, so
	// the CLI skips the legacy dirs; external runtimes run without it.
	return resolveModelDir(health.InstallationRoot, health.Exe, m.Owns(health.Exe), home), nil
}

// resolveModelDir mirrors crates/core-sdk business::resolve_model_dir.
func resolveModelDir(installationRoot, exe string, managed bool, home string) string {
	user := filepath.Join(installationRoot, "models", "bge-m3")
	candidates := []string{user}
	if !managed {
		candidates = append(candidates,
			filepath.Join(home, ".respire", "models", "bge-m3"),
			filepath.Join(home, ".onememory", "models", "bge-m3"),
		)
	}
	if exe != "" {
		candidates = append(candidates, filepath.Join(filepath.Dir(exe), "models", "bge-m3"))
	}
	for _, candidate := range candidates {
		if modelFilesPresent(candidate) {
			return candidate
		}
	}
	return user
}

// modelFilesPresent matches the CLI's baseline: tokenizer.json plus the
// quantized onnx graph.
func modelFilesPresent(dir string) bool {
	if info, err := os.Stat(filepath.Join(dir, "tokenizer.json")); err != nil || info.IsDir() {
		return false
	}
	info, err := os.Stat(filepath.Join(dir, variantFileName("quantized")))
	return err == nil && !info.IsDir()
}

// installedVariants returns the legacy M3 weights with the exact pinned size.
func installedVariants(dir string) map[string]int64 {
	present := map[string]int64{}
	for _, variant := range modelVariantFiles {
		if info, err := os.Stat(filepath.Join(dir, variantFileName(variant.Name))); err == nil && info.Size() == variant.Size {
			present[variant.Name] = info.Size()
		}
	}
	return present
}

// ModelVariants reports install state and location for every variant of one
// catalog model, powering the client's model table. m3 keeps the legacy
// resolution and CLI variant table; profile models read models/<id>.
func (m *Manager) ModelVariants(ctx context.Context, model string) (map[string]any, error) {
	definition, err := modelDefinitionByID(model)
	if err != nil {
		return nil, err
	}
	var dir string
	if definition.Legacy {
		dir, err = m.modelDir(ctx)
		if err != nil {
			return nil, err
		}
	} else {
		dir = m.ProfileDir(model)
	}
	var managedWeights, managedTokenizer string
	var managed bool
	if definition.ID == "m3" {
		if health, supported, err := m.runtimeModelInstallCapability(ctx); err == nil && supported {
			managedWeights, managedTokenizer, managed = m.managedM3Pair(health)
		}
	}
	variants := make([]map[string]any, 0, len(definition.Variants))
	for _, variant := range definition.Variants {
		path := filepath.Join(dir, definition.weightPath(variant.Name))
		entry := map[string]any{
			"name":      variant.Name,
			"size":      variant.Size,
			"path":      path,
			"bytes":     0,
			"installed": false,
		}
		if info, err := os.Stat(path); err == nil && info.Size() == variant.Size {
			entry["bytes"], entry["installed"] = variant.Size, true
		}
		if managed && variant.Name == "quantized" {
			entry["path"], entry["bytes"], entry["installed"], entry["runtime_managed"] = managedWeights, variant.Size, true, true
		}
		variants = append(variants, entry)
	}
	return map[string]any{
		"model":     definition.ID,
		"model_dir": dir,
		"tokenizer_installed": (managed && modelFileValid(managedTokenizer, definition.Tokenizer)) ||
			modelFileValid(filepath.Join(dir, "tokenizer.json"), definition.Tokenizer),
		"cpu_container_ready": !definition.Legacy || m.cpuContainerPresent(dir),
		"cpu_container_size":  m3CPUContainer.size,
		"variants":            variants,
	}, nil
}

type inferenceSelection struct {
	Engine string
	Model  string
}

func defaultVariantFor(engine string) string {
	if engine == "gpu" || engine == "npu" {
		return "fp16"
	}
	return "quantized"
}

func (m *Manager) readInferenceSelection() inferenceSelection {
	data, err := os.ReadFile(filepath.Join(m.DataRoot, "inference.json"))
	if err != nil {
		return inferenceSelection{}
	}
	var parsed struct {
		Engine   string `json:"engine"`
		Model    string `json:"model"`
		ForceCPU bool   `json:"force_cpu"`
	}
	if json.Unmarshal(data, &parsed) != nil {
		return inferenceSelection{}
	}
	engine := strings.ToLower(strings.TrimSpace(parsed.Engine))
	if parsed.ForceCPU || engine == "" || engine == "auto" {
		engine = "cpu"
	}
	return inferenceSelection{Engine: engine, Model: strings.TrimSpace(parsed.Model)}
}

func (m *Manager) EngineSnapshot(installationRoot, exe string) map[string]any {
	home, err := os.UserHomeDir()
	if err != nil {
		home = ""
	}
	if installationRoot == "" && home != "" {
		installationRoot = filepath.Join(home, ".rsrs")
	}
	dir := resolveModelDir(installationRoot, exe, m.Owns(exe), home)
	selection := m.readInferenceSelection()
	engine := selection.Engine
	if engine == "" {
		engine = "cpu"
	}
	variant, note := selection.Model, ""
	installed := installedVariants(dir)
	if variant == "" {
		variant = defaultVariantFor(engine)
		if engine != "cpu" && installed[variant] == 0 && installed["quantized"] != 0 {
			note = i18n.Message000cd5b14a20fcfc + variant + i18n.Message1c535a5ffbbdf556
			variant = "quantized"
		}
	}
	_, variantInstalled := installed[variant]
	if !variantInstalled && note == "" {
		note = i18n.Message6dee3c4aa4702705
	}
	file := variantFileName(variant)
	return map[string]any{
		"source":            "legacy",
		"engine":            engine,
		"variant":           variant,
		"file":              file,
		"path":              filepath.Join(dir, file),
		"model_dir":         dir,
		"variant_installed": variantInstalled,
		"note":              note,
	}
}
