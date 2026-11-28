package desktop

import (
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"runtime"
	"strings"
	"sync/atomic"
	"testing"

	"respire-client2/internal/host"
)

func TestDesktopModelActions(t *testing.T) {
	const model = "granite-97m-r2"
	const variant = "quint8_avx2"
	const probeBody = `{"model":"granite-97m-r2","variant":"quint8_avx2","engine":"npu"}`
	const deleteBody = `{"model":"granite-97m-r2","variant":"quint8_avx2"}`
	var probeCalls atomic.Int32
	var probeFailure, active atomic.Bool
	root := t.TempDir()
	modelDir := filepath.Join(root, "models", model)
	weights := filepath.Join(modelDir, "onnx", "model_quint8_avx2.onnx")
	tokenizer := filepath.Join(modelDir, "tokenizer.json")
	if err := os.MkdirAll(filepath.Dir(weights), 0700); err != nil {
		t.Fatal(err)
	}
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/api/health" {
			json.NewEncoder(w).Encode(host.HealthInfo{Server: "respire", Protocol: 1, PID: 42,
				AccountID: "account", InstanceID: "instance", Generation: 7, InstallationRoot: root})
			return
		}
		var request struct {
			Method    string   `json:"method"`
			Args      []string `json:"args"`
			Operation string   `json:"operation_id"`
		}
		if r.URL.Path != "/api/rpc" {
			t.Errorf("unexpected route: %s", r.URL.Path)
		}
		if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
			t.Error(err)
			return
		}
		switch request.Method {
		case "model.probe":
			probeCalls.Add(1)
			if len(request.Args) != 1 || request.Operation != "" {
				t.Errorf("probe must be a read-only RPC: %+v", request)
				return
			}
			var args struct {
				Model      string   `json:"model"`
				Variant    string   `json:"variant"`
				Engine     string   `json:"engine"`
				Weights    string   `json:"weights_path"`
				Tokenizer  string   `json:"tokenizer_path"`
				Companions []string `json:"companion_paths"`
				Dimensions int      `json:"dimensions"`
				MaxTokens  int      `json:"max_tokens"`
				Query      *string  `json:"query_prefix"`
				PadID      int      `json:"pad_token_id"`
				Granite    bool     `json:"granite"`
			}
			if err := json.Unmarshal([]byte(request.Args[0]), &args); err != nil {
				t.Error(err)
			}
			if args.Model != model || args.Variant != variant || args.Engine != "npu" || strings.TrimPrefix(args.Weights, `\\?\`) != weights ||
				strings.TrimPrefix(args.Tokenizer, `\\?\`) != tokenizer || args.Companions == nil || len(args.Companions) != 0 || args.Dimensions != 384 ||
				args.MaxTokens != 32768 || args.Query == nil || *args.Query != "" || args.PadID != 179935 || !args.Granite {
				t.Errorf("wrong probe args: %+v", args)
			}
			if probeFailure.Load() {
				io.WriteString(w, `{"ok":true,"envelope":{"status":"ok","summary":{"ok":false,"error_code":"provider_failed","diagnostic_log":"provider unavailable"}}}`)
			} else {
				io.WriteString(w, `{"ok":true,"envelope":{"status":"ok","summary":{"ok":true,"dimensions":384,"elapsed_ms":9}}}`)
			}
		case "model.progress":
			io.WriteString(w, `{"ok":true,"envelope":{"summary":{"switch":{},"operation":{"active":false},"pending_selection":null}}}`)
		case "model.catalog":
			if active.Load() {
				io.WriteString(w, `{"ok":true,"envelope":{"summary":{"active":{"model":"granite-97m-r2","variant":"quint8_avx2"}}}}`)
			} else {
				io.WriteString(w, `{"ok":true,"envelope":{"summary":{"active":{"model":"granite-97m-r2","variant":"standard"}}}}`)
			}
		default:
			t.Errorf("candidate operation attempted a mutation: %s", request.Method)
			http.NotFound(w, r)
		}
	}))
	defer server.Close()
	_, port, _ := strings.Cut(server.URL, "127.0.0.1:")
	t.Setenv("RSRS_RPC_PORT", port)
	t.Setenv("RSRS_DATA_DIR", root)
	manager, err := host.New()
	if err != nil {
		t.Fatal(err)
	}
	manager.ClientVersion = "fixture-client"
	d := &desktop{m: manager}
	handler := d.handler(http.NotFoundHandler())
	request := func(path, body string, authorized bool, status int) *httptest.ResponseRecorder {
		t.Helper()
		req := httptest.NewRequest(http.MethodPost, path, strings.NewReader(body))
		if authorized {
			req.Header.Set("X-Respire-Client", "1")
		}
		res := httptest.NewRecorder()
		handler.ServeHTTP(res, req)
		if res.Code != status {
			t.Fatalf("%s status=%d, want %d: %s", path, res.Code, status, res.Body)
		}
		return res
	}
	request("/api/model-probe", probeBody, false, http.StatusForbidden)
	request("/api/model-delete", deleteBody, false, http.StatusForbidden)
	request("/api/model-probe", strings.Replace(probeBody, "npu", "auto", 1), true, http.StatusBadRequest)
	request("/api/model-probe", probeBody, true, http.StatusNotFound)
	request("/api/model-delete", `{"model":"../other","variant":"standard"}`, true, http.StatusBadRequest)
	if probeCalls.Load() != 0 {
		t.Fatal("invalid or missing candidate was sent to Runtime")
	}
	for path, size := range map[string]int64{weights: 98247878, tokenizer: 25301672} {
		file, err := os.Create(path)
		if err != nil {
			t.Fatal(err)
		}
		growErr := file.Truncate(size)
		if err := errors.Join(growErr, file.Close()); err != nil {
			t.Fatal(err)
		}
	}
	active.Store(true)
	for _, fixture := range []struct {
		variant string
		ok      bool
		message string
	}{{"", true, "elapsed_ms"}, {"quantized", false, "provider unavailable"}} {
		probeFailure.Store(!fixture.ok)
		res := request("/api/model-probe", strings.Replace(probeBody, variant, fixture.variant, 1), true, http.StatusOK)
		var reply struct {
			OK     bool                  `json:"ok"`
			Result host.ModelProbeResult `json:"result"`
		}
		if err := json.Unmarshal(res.Body.Bytes(), &reply); err != nil || !reply.OK || reply.Result.OK != fixture.ok || reply.Result.Variant != variant ||
			reply.Result.ClientVersion != manager.ClientVersion || reply.Result.OS != runtime.GOOS || reply.Result.Arch != runtime.GOARCH ||
			!strings.Contains(string(reply.Result.Diagnostics), fixture.message) {
			t.Fatalf("probe diagnostics: %s (%v)", res.Body, err)
		}
	}
	request("/api/model-delete", strings.Replace(deleteBody, variant, "", 1), true, http.StatusConflict)
	if _, err := os.Stat(weights); err != nil {
		t.Fatalf("probe or rejected delete touched incumbent: %v", err)
	}
	active.Store(false)
	res := request("/api/model-delete", strings.Replace(deleteBody, variant, "quantized", 1), true, http.StatusOK)
	var reply struct {
		OK     bool              `json:"ok"`
		Result map[string]string `json:"result"`
	}
	if err := json.Unmarshal(res.Body.Bytes(), &reply); err != nil || !reply.OK || reply.Result["variant"] != variant ||
		reply.Result["model"] != model || reply.Result["status"] != "deleted" {
		t.Fatalf("delete response: %s (%v)", res.Body, err)
	}
	if _, err := os.Stat(weights); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("inactive weight remains: %v", err)
	}
	if _, err := os.Stat(tokenizer); err != nil {
		t.Fatalf("shared tokenizer was removed: %v", err)
	}
}

func TestAllowModelRevealPath(t *testing.T) {
	root := t.TempDir()
	m3Dir := filepath.Join(root, "models", "bge-m3")
	for _, path := range []string{
		filepath.Join(m3Dir, "onnx", "model_quantized.onnx"),
		filepath.Join(root, "models", "granite-311m-r2", "onnx", "model_quint8_avx2.onnx"),
		filepath.Join(root, "models", "granite-97m-r2", "onnx", "model.onnx"),
		filepath.Join(root, "models", "bge-base-zh-v1.5", "onnx", "model_fp16.onnx"),
	} {
		if _, ok, err := allowModelRevealPath(root, m3Dir, path); err != nil || !ok {
			t.Fatalf("expected reveal path allowed for %s: ok=%v err=%v", path, ok, err)
		}
	}
	for _, blocked := range []string{
		"",
		"   ",
		filepath.Join(root, "client2", "preferences.json"),
		filepath.Join(root, "models", "..", "rsrs.db"),
	} {
		if _, ok, err := allowModelRevealPath(root, m3Dir, blocked); err != nil || ok {
			t.Fatalf("expected reveal path blocked for %s: ok=%v err=%v", blocked, ok, err)
		}
	}
}
