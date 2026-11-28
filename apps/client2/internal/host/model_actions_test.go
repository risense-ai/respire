package host

import (
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"sync/atomic"
	"testing"
	"time"
)

func TestCuratedModelCatalog(t *testing.T) {
	want := map[string]map[string]int64{
		DefaultModelID:   {"quint8_avx2": 313421909, "standard": 1247170481},
		"granite-97m-r2": {"quint8_avx2": 98247878, "standard": 390004608},
		"m3": {"quantized": 569694530, "fp16": 1133992936, "int8": 568456694, "uint8": 568456765,
			"q4": 1248237611, "q4f16": 699932840, "bnb4": 1229364419, "fp32": 607298},
		"bge-base-zh-v1.5": {"quantized": 102868746, "fp16": 203679052, "int8": 102407793, "uint8": 102407824,
			"q4": 120342974, "q4f16": 81549931, "bnb4": 115035086, "fp32": 406953171},
	}
	if len(modelCatalog) != len(want) || modelCatalog[0].ID != "granite-311m-r2" {
		t.Fatal("unexpected catalog or fresh-install default")
	}
	for _, definition := range modelCatalog {
		actual := map[string]int64{}
		for _, variant := range definition.Variants {
			files, err := definition.weightFiles(variant.Name)
			if err != nil || len(files[0].sha256) != 64 || filepath.ToSlash(files[0].rel) != filepath.ToSlash(variantFileName(variant.Name)) {
				t.Fatalf("%s/%s: %v %v", definition.ID, variant.Name, files, err)
			}
			actual[variant.Name] = files[0].size
			if definition.ID == "m3" && variant.Name == "fp32" {
				if len(files) != 2 || filepath.ToSlash(files[1].rel) != "onnx/model.onnx_data" || files[1].size != 2266820608 {
					t.Fatalf("external weights: %+v", files)
				}
			} else if len(files) != 1 {
				t.Fatalf("unexpected sidecar: %+v", files)
			}
		}
		if !reflect.DeepEqual(actual, want[definition.ID]) || definition.Granite &&
			(definition.selectedVariant("") != "quint8_avx2" || definition.selectedVariant("quantized") != "quint8_avx2") {
			t.Errorf("%s: %v", definition.ID, actual)
		}
	}
	if _, err := modelDefinitionByID("bge-m3-distill-8l"); err == nil {
		t.Fatal("legacy model still selectable")
	}
}

func TestModelCatalogPreservesLegacyActiveSelection(t *testing.T) {
	manager := v1TestManager(t, []string{"model.catalog.v2"}, func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/v1/models/catalog" {
			io.WriteString(w, `{"active_model_id":"bge-m3-distill-8l","models":[{"id":"bge-m3-distill-8l"},{"id":"m3"}]}`)
			return
		}
		io.WriteString(w, `{"ok":true,"envelope":{"summary":{"active":{"model":"bge-m3-distill-8l","variant":"fp16"}}}}`)
	})
	raw, err := manager.ModelCatalogRPC(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var result struct {
		DefaultModel string `json:"default_model"`
		Selectable   []struct {
			ID string `json:"id"`
		} `json:"selectable_models"`
		Catalog struct {
			Models []struct {
				ID string `json:"id"`
			} `json:"models"`
		} `json:"catalog_v1"`
		Summary struct {
			Active modelVariant `json:"active"`
		} `json:"summary"`
	}
	if err := json.Unmarshal(raw, &result); err != nil {
		t.Fatal(err)
	}
	if result.DefaultModel != DefaultModelID || len(result.Selectable) != 4 || len(result.Catalog.Models) != 1 ||
		result.Catalog.Models[0].ID != "m3" || result.Summary.Active != (modelVariant{Model: "bge-m3-distill-8l", Variant: "fp16"}) {
		t.Fatalf("catalog: %s", raw)
	}
}

func TestModelSwitchSendsExactSelection(t *testing.T) {
	for _, selection := range []struct{ model, variant, engine, file string }{
		{DefaultModelID, "quint8_avx2", "cpu", "model_quint8_avx2.onnx"},
		{"granite-311m-r2", "standard", "gpu", "model.onnx"},
		{"granite-97m-r2", "quint8_avx2", "npu", "model_quint8_avx2.onnx"},
		{"granite-97m-r2", "standard", "cpu", "model.onnx"},
		{"m3", "q4f16", "gpu", "model_q4f16.onnx"},
		{"m3", "fp32", "cpu", "model.onnx"},
		{"m3", "bnb4", "npu", "model_bnb4.onnx"},
		{"bge-base-zh-v1.5", "fp32", "npu", "model.onnx"},
	} {
		t.Run(selection.model+"/"+selection.variant, func(t *testing.T) {
			var configured, switched atomic.Bool
			manager := v1TestManager(t, []string{"openapi.v1"}, func(w http.ResponseWriter, r *http.Request) {
				var payload map[string]any
				if err := json.NewDecoder(r.Body).Decode(&payload); err != nil {
					t.Error(err)
					return
				}
				if r.URL.Path == "/api/rpc" {
					method := payload["method"]
					args := payload["args"].([]any)
					if method == "runtime.result" {
						fmt.Fprintf(w, `{"ok":true,"envelope":{"summary":{"operation_id":%q,"state":"completed","response":{"ok":true}}}}`, args[0])
						return
					}
					if err := json.Unmarshal([]byte(args[0].(string)), &payload); err != nil {
						t.Error(err)
						return
					}
					if method == "model.configure" {
						profile := payload["model_profile"].(map[string]any)
						weights := profile["weights"].(map[string]any)
						if profile["id"] != selection.model || filepath.Base(weights["path"].(string)) != selection.file {
							t.Errorf("wrong registered profile: %v", profile)
						}
						if selection.model == "m3" && selection.variant == "fp32" {
							companions, ok := profile["companions"].([]any)
							if !ok || len(companions) != 1 || filepath.Base(companions[0].(map[string]any)["path"].(string)) != "model.onnx_data" {
								t.Errorf("missing external weights in profile: %v", profile)
							}
						}
						configured.Store(true)
						io.WriteString(w, `{"ok":true,"envelope":{"status":"ok"}}`)
						return
					}
					if method != "model.switch" || payload["model"] != selection.model {
						t.Errorf("wrong RPC selection: %v", payload)
					}
					scope, err := json.Marshal(payload["scope"])
					if err != nil {
						t.Error(err)
						return
					}
					fmt.Fprintf(w, `{"v":1,"id":%q,"operation_id":%q,"scope":%s,"ok":true,"envelope":{"status":"pending"}}`,
						payload["id"], payload["operation_id"], scope)
				} else {
					if r.URL.Path != "/v1/models/switch" || payload["model_id"] != selection.model {
						t.Errorf("wrong V1 selection: %v", payload)
					}
					w.WriteHeader(http.StatusAccepted)
					io.WriteString(w, `{"operation_id":"switch","status":"accepted"}`)
				}
				if !configured.Load() || payload["variant"] != selection.variant || payload["engine"] != selection.engine {
					t.Errorf("incomplete selection: %v", payload)
				}
				switched.Store(true)
			})
			receipt, err := manager.StartModelSwitch(t.Context(), selection.model, selection.engine, selection.variant)
			if err != nil || !switched.Load() || receipt.OperationAccepted == nil {
				t.Fatalf("switch: %v", err)
			}
			if selection.engine != "cpu" {
				raw, err := manager.RuntimeOperation(t.Context(), receipt.OperationId, false)
				if err != nil || !strings.Contains(string(raw), `"state":"completed"`) {
					t.Fatalf("RPC switch status: %s, %v", raw, err)
				}
			}
		})
	}
}

func TestModelDownloadVerifiesCompanionAndBlocksDeleteWhileRunning(t *testing.T) {
	definition, err := modelDefinitionByID("m3")
	if err != nil {
		t.Fatal(err)
	}
	files, err := definition.weightFiles("fp32")
	if err != nil {
		t.Fatal(err)
	}
	originalTokenizer := modelCatalog[2].Tokenizer
	t.Cleanup(func() {
		modelCatalog[2].Tokenizer = originalTokenizer
		modelFileSpecs["fp32"] = files[0]
		m3ExternalData = files[1]
	})
	fixture := func(spec modelFileSpec) modelFileSpec {
		return modelFileSpec{rel: spec.rel, size: int64(len(spec.rel)), sha256: fmt.Sprintf("%x", sha256.Sum256([]byte(spec.rel)))}
	}
	modelCatalog[2].Tokenizer = fixture(originalTokenizer)
	modelFileSpecs["fp32"], m3ExternalData = fixture(files[0]), fixture(files[1])
	companionStarted := make(chan struct{})
	release := make(chan struct{})
	var received atomic.Int32
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		const prefix = "/Xenova/bge-m3/resolve/" + modelRevision + "/"
		file, found := strings.CutPrefix(r.URL.Path, prefix)
		if !found {
			t.Errorf("unexpected mirror path %s", r.URL)
		}
		if file == m3ExternalData.rel {
			close(companionStarted)
			<-release
		}
		received.Add(1)
		w.WriteHeader(http.StatusPartialContent)
		io.WriteString(w, file)
	}))
	defer server.Close()
	defer func() {
		select {
		case <-release:
		default:
			close(release)
		}
	}()
	manager := &Manager{DataRoot: t.TempDir(), modelHTTPClient: server.Client(), prefs: Preferences{ModelMirror: server.URL}}
	if _, err := manager.StartModelDownload("m3", "fp32", server.URL+"/invalid"); !errors.Is(err, ErrInvalidModelVariant) {
		t.Fatalf("invalid origin: %v", err)
	}
	id, err := manager.StartModelDownload("m3", "fp32", "")
	if err != nil || id == "" {
		t.Fatalf("download: %q %v", id, err)
	}
	select {
	case <-companionStarted:
	case <-time.After(5 * time.Second):
		t.Fatal("mirror did not receive external data request")
	}
	if _, err := manager.DeleteModelVariant(context.Background(), "m3", "fp32"); !errors.Is(err, ErrModelVariantBusy) {
		t.Errorf("in-flight download deletion: %v", err)
	}
	close(release)
	for task, changed, found := manager.WatchTask(id); ; task, changed, found = manager.WatchTask(id) {
		if !found {
			t.Fatal("lost download task")
		}
		if !task.Busy {
			if task.State != "downloaded" || task.Error != "" || received.Load() != 3 || !modelFileValid(filepath.Join(manager.ModelDir(), m3ExternalData.rel), m3ExternalData) {
				t.Fatalf("download did not settle with verified companion: %+v", task)
			}
			break
		}
		select {
		case <-changed:
		case <-time.After(5 * time.Second):
			t.Fatal("download did not settle")
		}
	}
}

func TestDeleteModelVariantRemovesFP32PairOnly(t *testing.T) {
	var progress atomic.Value
	manager := v1TestManager(t, nil, func(w http.ResponseWriter, r *http.Request) {
		var request map[string]any
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			t.Error(err)
		}
		switch request["method"] {
		case "model.progress":
			fmt.Fprintf(w, `{"ok":true,"envelope":{"summary":%s}}`, progress.Load())
		case "model.catalog":
			io.WriteString(w, `{"ok":true,"envelope":{"summary":{"active":{"model":"m3","variant":"quantized"}}}}`)
		default:
			t.Errorf("unexpected model mutation: %v", request)
		}
	})
	dir := manager.ModelDir()
	if err := os.MkdirAll(filepath.Join(dir, "onnx"), 0700); err != nil {
		t.Fatal(err)
	}
	graph, companion, tokenizer, active := filepath.Join(dir, "onnx", "model.onnx"), filepath.Join(dir, m3ExternalData.rel),
		filepath.Join(dir, "tokenizer.json"), filepath.Join(dir, variantFileName("quantized"))
	for _, file := range []string{graph, companion, tokenizer, active} {
		if err := os.WriteFile(file, []byte("fixture"), 0600); err != nil {
			t.Fatal(err)
		}
	}
	for _, fixture := range []struct{ name, progress string }{
		{"pending selection", `{"switch":{},"operation":{"active":false},"pending_selection":{"model":"m3","variant":"fp32"}}`},
		{"new switch with stale selection", `{"switch":{"state":"pending","target":"m3"},"operation":{"active":true},"pending_selection":{"model":"granite-97m-r2","variant":"quint8_avx2"}}`},
		{"active operation with stale selection", `{"switch":{"state":"interrupted"},"operation":{"active":true},"pending_selection":{"model":"granite-97m-r2","variant":"quint8_avx2"}}`},
	} {
		t.Run(fixture.name, func(t *testing.T) {
			progress.Store(fixture.progress)
			if _, err := manager.DeleteModelVariant(t.Context(), "m3", "fp32"); !errors.Is(err, ErrModelVariantBusy) {
				t.Fatalf("deletion was not blocked: %v", err)
			}
			if _, err := os.Stat(graph); err != nil {
				t.Fatalf("blocked deletion touched weights: %v", err)
			}
		})
	}
	progress.Store(`{"switch":{},"operation":{"active":false},"pending_selection":null}`)
	if _, err := manager.DeleteModelVariant(t.Context(), "m3", "quantized"); !errors.Is(err, ErrModelVariantBusy) {
		t.Fatalf("active variant deletion: %v", err)
	}
	if variant, err := manager.DeleteModelVariant(t.Context(), "m3", "fp32"); err != nil || variant != "fp32" {
		t.Fatalf("delete returned %q: %v", variant, err)
	}
	for _, file := range []string{graph, companion} {
		if _, err := os.Stat(file); !errors.Is(err, os.ErrNotExist) {
			t.Errorf("deleted file remains: %s, %v", file, err)
		}
	}
	for _, file := range []string{tokenizer, active} {
		if _, err := os.Stat(file); err != nil {
			t.Errorf("retained file missing: %s, %v", file, err)
		}
	}
}

func TestProbeM3RequiresExternalWeights(t *testing.T) {
	tokenizer, graph, external := modelCatalog[2].Tokenizer, modelFileSpecs["fp32"], m3ExternalData
	t.Cleanup(func() {
		modelCatalog[2].Tokenizer, modelFileSpecs["fp32"], m3ExternalData = tokenizer, graph, external
	})
	tokenizerFixture, graphFixture, externalFixture := tokenizer, graph, external
	tokenizerFixture.size, graphFixture.size, externalFixture.size = 1, 1, 1
	modelCatalog[2].Tokenizer, modelFileSpecs["fp32"], m3ExternalData = tokenizerFixture, graphFixture, externalFixture
	var called atomic.Bool
	manager := v1TestManager(t, nil, func(w http.ResponseWriter, r *http.Request) {
		var request struct {
			Method string   `json:"method"`
			Args   []string `json:"args"`
		}
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil || request.Method != "model.probe" || len(request.Args) != 1 {
			t.Errorf("unexpected RPC: %+v (%v)", request, err)
			return
		}
		var input struct {
			Weights    string   `json:"weights_path"`
			Companions []string `json:"companion_paths"`
		}
		if err := json.Unmarshal([]byte(request.Args[0]), &input); err != nil || filepath.Base(input.Weights) != "model.onnx" || len(input.Companions) != 1 ||
			input.Companions[0] != filepath.Join(filepath.Dir(input.Weights), "model.onnx_data") {
			t.Errorf("unexpected candidate paths: %+v (%v)", input, err)
		}
		called.Store(true)
		io.WriteString(w, `{"ok":true,"envelope":{"status":"ok","summary":{"ok":true}}}`)
	})
	manager.localClient.Timeout = 30 * time.Second
	transport := manager.localClient.Transport
	manager.localClient.Transport = modelActionTransport(func(r *http.Request) (*http.Response, error) {
		deadline, hasDeadline := r.Context().Deadline()
		remaining := time.Until(deadline)
		if r.URL.Path == "/api/rpc" {
			if !hasDeadline || remaining < 150*time.Second || remaining > 180*time.Second {
				t.Errorf("probe HTTP deadline has %s remaining, want 150s to 180s", remaining)
			}
		} else if !hasDeadline || remaining <= 0 || remaining > 30*time.Second {
			t.Errorf("health HTTP deadline has %s remaining, want at most 30s", remaining)
		}
		return transport.RoundTrip(r)
	})
	dir := manager.ModelDir()
	if err := os.MkdirAll(filepath.Join(dir, "onnx"), 0700); err != nil {
		t.Fatal(err)
	}
	writeFixture := func(spec modelFileSpec) {
		t.Helper()
		if err := os.WriteFile(filepath.Join(dir, spec.rel), []byte{0}, 0600); err != nil {
			t.Fatal(err)
		}
	}
	writeFixture(tokenizerFixture)
	writeFixture(graphFixture)
	if _, err := manager.ProbeModelVariant(t.Context(), "m3", "fp32", "cpu"); !errors.Is(err, ErrModelFilesMissing) || called.Load() {
		t.Fatalf("probe accepted missing external weights: %v", err)
	}
	writeFixture(externalFixture)
	result, err := manager.ProbeModelVariant(t.Context(), "m3", "fp32", "cpu")
	if err != nil || !result.OK || !called.Load() {
		t.Fatalf("probe did not use complete variant: %+v %v", result, err)
	}
	if manager.localClient.Timeout != 30*time.Second {
		t.Fatalf("probe changed shared HTTP timeout to %s", manager.localClient.Timeout)
	}
}

type modelActionTransport func(*http.Request) (*http.Response, error)

func (transport modelActionTransport) RoundTrip(r *http.Request) (*http.Response, error) {
	return transport(r)
}

func TestDeleteModelVariantNormalizesGraniteAliases(t *testing.T) {
	const model, variant = "granite-97m-r2", "quint8_avx2"
	manager := v1TestManager(t, nil, func(w http.ResponseWriter, r *http.Request) {
		var request struct {
			Method string `json:"method"`
		}
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			t.Error(err)
			return
		}
		switch request.Method {
		case "model.progress":
			io.WriteString(w, `{"ok":true,"envelope":{"summary":{"switch":{},"operation":{"active":false},"pending_selection":null}}}`)
		case "model.catalog":
			io.WriteString(w, `{"ok":true,"envelope":{"summary":{"active":{"model":"granite-97m-r2","variant":"standard"}}}}`)
		default:
			t.Errorf("unexpected RPC %q", request.Method)
		}
	})
	weights := filepath.Join(manager.ProfileDir(model), variantFileName(variant))
	if err := os.MkdirAll(filepath.Dir(weights), 0700); err != nil {
		t.Fatal(err)
	}
	key := modelVariant{Model: model, Variant: variant}
	for _, alias := range []string{"", "quantized"} {
		if err := os.WriteFile(weights, []byte("fixture"), 0600); err != nil {
			t.Fatal(err)
		}
		manager.reserveModelDownload(key)
		if _, err := manager.DeleteModelVariant(t.Context(), model, alias); !errors.Is(err, ErrModelVariantBusy) {
			t.Fatalf("deletion with alias %q did not recognize active download: %v", alias, err)
		}
		manager.releaseModelDownload(key)
		if got, err := manager.DeleteModelVariant(t.Context(), model, alias); err != nil || got != variant {
			t.Fatalf("deletion with alias %q returned %q: %v", alias, got, err)
		}
		if _, err := os.Stat(weights); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("deletion with alias %q left weights: %v", alias, err)
		}
	}
}
