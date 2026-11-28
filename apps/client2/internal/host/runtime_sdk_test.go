package host

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestRuntimeSDKCapabilityNegotiation(t *testing.T) {
	for _, tc := range []struct {
		name               string
		status             int
		body               string
		supported, failure bool
	}{
		{"404 legacy", 404, "", false, false},
		{"no flags", 200, `{"status":"ready","instance_id":"i","capabilities":[]}`, false, false},
		{"capability without version", 200, `{"status":"ready","instance_id":"i","capabilities":["openapi.v1"]}`, true, false},
		{"500 is not legacy", 500, `{}`, false, true},
		{"401 is not legacy", 401, `{}`, false, true},
		{"invalid health", 200, `{}`, false, true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.URL.Path != "/v1/health" {
					t.Errorf("unexpected request %s", r.URL.Path)
				}
				w.WriteHeader(tc.status)
				io.WriteString(w, tc.body)
			}))
			defer server.Close()
			manager := &Manager{Endpoint: server.URL, localClient: server.Client()}
			_, supported, err := manager.runtimeSDK(context.Background(), "openapi.v1")
			if supported != tc.supported || (err != nil) != tc.failure {
				t.Fatalf("supported=%v err=%v", supported, err)
			}
		})
	}
}

func TestRuntimeSDKModelHandlersAndCancellation(t *testing.T) {
	var switched, downloaded, cancelled, snapshots, assets bool
	manager := v1TestManager(t, []string{"openapi.v1", "model.managed_download", "model.active_assets", "sse.operations.v1"}, func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/v1/models/switch":
			switched = true
			var body map[string]string
			if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
				t.Error(err)
			}
			if r.Method != "POST" || body["model_id"] != "m3" || body["engine"] != "cpu" || body["variant"] != "quantized" {
				t.Errorf("invalid switch: %v", body)
			}
			w.WriteHeader(202)
			io.WriteString(w, `{"operation_id":"switch","status":"accepted"}`)
		case "/v1/models/downloads":
			downloaded = true
			var body map[string]string
			if err := json.NewDecoder(r.Body).Decode(&body); err != nil {
				t.Error(err)
			}
			if body["model_id"] != "m3" || body["mirror"] != "https://hf-mirror.com" {
				t.Errorf("invalid download: %v", body)
			}
			if _, found := body["variant"]; found {
				t.Error("invented download variant field")
			}
			w.WriteHeader(202)
			io.WriteString(w, `{"operation_id":"download","status":"accepted"}`)
		case "/v1/operations/switch":
			if r.Method == "DELETE" {
				cancelled = true
				w.WriteHeader(202)
			} else {
				snapshots = true
			}
			io.WriteString(w, `{"operation_id":"switch","kind":"model_switch","state":"cancelled"}`)
		case "/v1/models/active-assets":
			assets = true
			io.WriteString(w, `{"active_model_id":"m3","active_paths":[]}`)
		default:
			t.Errorf("unexpected route %s", r.URL.Path)
			http.NotFound(w, r)
		}
	})
	ctx := context.Background()
	receipt, err := manager.StartModelSwitch(ctx, "m3", "cpu", "quantized")
	if err != nil || receipt.OperationId != "switch" || !receipt.Stream {
		t.Fatalf("switch=%v err=%v", receipt, err)
	}
	download, handled, err := manager.StartRuntimeModelDownload(ctx, "m3", "quantized", "https://hf-mirror.com")
	if err != nil || !handled || download.OperationId != "download" || !download.Stream {
		t.Fatalf("download=%v handled=%v err=%v", download, handled, err)
	}
	for _, cancel := range []bool{false, true} {
		if _, err := manager.RuntimeOperation(ctx, "switch", cancel); err != nil {
			t.Fatal(err)
		}
	}
	if _, err := manager.ActiveModelAssets(ctx); err != nil {
		t.Fatal(err)
	}
	if !switched || !downloaded || !cancelled || !snapshots || !assets {
		t.Fatal("a migrated SDK route was skipped")
	}
	for _, variant := range []string{"baseline", "fp16"} {
		if _, handled, err := manager.StartRuntimeModelDownload(ctx, "m3", variant, ""); err != nil || handled {
			t.Fatalf("legacy variant %s was migrated: %v", variant, err)
		}
	}
}

func TestModelCancelRoutesToRuntimeSwitchOrRPCWorker(t *testing.T) {
	for _, target := range []string{"v1-switch", "rpc-switch"} {
		t.Run(target, func(t *testing.T) {
			const id = "desktop-20000000-0000-4000-8000-000000000001"
			manager := v1TestManager(t, []string{"openapi.v1", "sse.operations.v1"}, func(w http.ResponseWriter, r *http.Request) {
				switch {
				case r.URL.Path == "/v1/operations/v1-switch" && target == "v1-switch" && r.Method == http.MethodGet:
					io.WriteString(w, `{"operation_id":"v1-switch","kind":"model_switch","state":"running"}`)
				case r.URL.Path == "/v1/operations/v1-switch" && target == "v1-switch" && r.Method == http.MethodDelete:
					w.WriteHeader(http.StatusAccepted)
					io.WriteString(w, `{"operation_id":"v1-switch","kind":"model_switch","state":"cancelled"}`)
				case r.URL.Path == "/v1/operations/rpc-switch" && target == "rpc-switch" && r.Method == http.MethodGet:
					http.NotFound(w, r)
				case r.URL.Path == "/api/rpc" && target == "rpc-switch" && r.Method == http.MethodPost:
					var request struct {
						Method string        `json:"method"`
						Args   []string      `json:"args"`
						Scope  *runtimeScope `json:"scope"`
					}
					if err := json.NewDecoder(r.Body).Decode(&request); err != nil || request.Method != "model.control" || strings.Join(request.Args, ",") != "rpc-switch,true" {
						t.Errorf("unexpected RPC cancellation request: %+v, %v", request, err)
						http.Error(w, "invalid cancellation request", http.StatusBadRequest)
						return
					}
					json.NewEncoder(w).Encode(rpcResponse{V: 1, ID: id, OK: true, Scope: request.Scope, OperationID: id,
						Envelope: json.RawMessage(`{"status":"ok","summary":{"id":"rpc-switch","active":true,"cancelled":true}}`)})
				default:
					t.Errorf("unexpected cancellation request: %s %s", r.Method, r.URL.Path)
					http.NotFound(w, r)
				}
			})
			manager.jobs = map[string]*Operation{id: {ID: id, Name: "model_cancel", State: "running", write: true}}
			spec, _ := operationSpec("model_cancel")
			manager.runOperation(spec, []string{target, "true"}, "", id, "model_cancel", json.RawMessage(fmt.Sprintf(`{"id":%q}`, target)))
			job, err := manager.Operation(id)
			if err != nil || job.State != "done" || job.RuntimeV1 != (target == "v1-switch") || !strings.Contains(string(job.Envelope), `"cancelled"`) {
				t.Fatalf("model cancellation job = %+v, error = %v", job, err)
			}
		})
	}
}

func TestRuntimeSDKSpacesSyncAndBackupHandlers(t *testing.T) {
	var requests []string
	manager := v1TestManager(t, []string{"openapi.v1", "backups.v1", "sse.operations.v1"}, func(w http.ResponseWriter, r *http.Request) {
		requests = append(requests, r.Method+" "+r.URL.Path)
		switch r.URL.Path {
		case "/api/rpc":
			writeSpaceStatus(t, w, r, false)
		case "/v1/spaces":
			if r.Method == "GET" {
				io.WriteString(w, `{"spaces":[{"id":"space-id","name":"work","role":"owner","active":true}]}`)
			} else {
				w.WriteHeader(201)
				io.WriteString(w, `{"id":"new-id","name":"new","role":"owner"}`)
			}
		case "/v1/spaces/space-id", "/v1/spaces/new-id", "/v1/spaces/main":
			if r.Method == "DELETE" {
				w.WriteHeader(204)
			} else {
				io.WriteString(w, `{"id":"space-id","name":"work","role":"owner","active":true}`)
			}
		case "/v1/sync/trigger", "/v1/backups/import":
			if r.URL.Path == "/v1/backups/import" {
				body, _ := io.ReadAll(r.Body)
				if string(body) != "opaque archive" || r.Header.Get("Content-Type") != "application/octet-stream" {
					t.Error("archive upload was changed")
				}
			}
			w.WriteHeader(202)
			io.WriteString(w, `{"operation_id":"op","status":"accepted"}`)
		case "/v1/operations/op/events":
			w.Header().Set("Content-Type", "text/event-stream")
			io.WriteString(w, operationFrame("completed", "completed", "op"))
		case "/v1/backups/export":
			w.Header().Set("Content-Type", "application/octet-stream")
			io.WriteString(w, "opaque archive")
		default:
			t.Errorf("unexpected request %s", r.URL.Path)
			http.NotFound(w, r)
		}
	})
	ctx := context.Background()
	path := filepath.Join(t.TempDir(), "fixture.rsrs-backup")
	for _, tc := range []struct{ name, args string }{
		{"space_list", `{}`}, {"space_create", `{"name":"new"}`}, {"space_use", `{"name":"v1:space-id"}`}, {"space_use", `{"name":"v1:main"}`}, {"space_remove", `{"name":"v1:space-id"}`}, {"sync", `{}`},
	} {
		raw, handled, err := manager.executeV1Operation(ctx, tc.name, "desktop-fixture", json.RawMessage(tc.args))
		if err != nil || !handled || !json.Valid(raw) {
			t.Fatalf("%s: handled=%v, err=%v raw=%s", tc.name, handled, err, raw)
		}
		if tc.name == "space_list" {
			var envelope struct {
				Summary struct {
					Spaces []struct {
						ID      string `json:"id"`
						Current bool   `json:"current"`
					} `json:"spaces"`
				} `json:"summary"`
			}
			if err := json.Unmarshal(raw, &envelope); err != nil {
				t.Fatal(err)
			}
			if len(envelope.Summary.Spaces) != 2 || envelope.Summary.Spaces[0].ID != "main" || envelope.Summary.Spaces[0].Current {
				t.Fatalf("original space restore entry is missing: %s", raw)
			}
		}
	}
	args, err := json.Marshal(map[string]string{"path": path})
	if err != nil {
		t.Fatal(err)
	}
	if _, handled, err := manager.executeV1Operation(ctx, "memory_backup", "desktop-fixture", args); err != nil || !handled {
		t.Fatalf("memory_backup: %v", err)
	}
	client, _, err := manager.runtimeSDK(ctx, "backups.v1")
	if err != nil {
		t.Fatal(err)
	}
	staged, err := manager.backupV1(ctx, client, path, true)
	if err != nil || !strings.Contains(string(staged), `"operation_id":"op"`) {
		t.Fatalf("staging: %s %v", staged, err)
	}
	if _, handled, err := manager.executeV1Operation(ctx, "memory_import", "desktop-fixture", args); !handled || err == nil || !strings.Contains(err.Error(), "native owner confirmation") {
		t.Fatalf("archive upload must not be presented as restoration: %v", err)
	}
	archive, err := os.ReadFile(path)
	if err != nil || string(archive) != "opaque archive" {
		t.Fatalf("archive=%q err=%v", archive, err)
	}
	for _, route := range []string{"POST /v1/spaces", "GET /v1/spaces", "PATCH /v1/spaces/space-id", "PATCH /v1/spaces/main", "DELETE /v1/spaces/space-id", "POST /v1/sync/trigger", "GET /v1/backups/export", "POST /v1/backups/import", "GET /v1/operations/op/events"} {
		if !strings.Contains(strings.Join(requests, "\n"), route) {
			t.Errorf("route was not used: %s", route)
		}
	}
}

func writeSpaceStatus(t *testing.T, w http.ResponseWriter, r *http.Request, remote bool) {
	t.Helper()
	var request map[string]any
	if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
		t.Error(err)
		return
	}
	json.NewEncoder(w).Encode(map[string]any{"v": 1, "id": request["id"], "scope": request["scope"], "operation_id": request["id"], "ok": true,
		"envelope": map[string]any{"command": "status", "status": "ok", "summary": map[string]any{"remote_configured": remote, "data_dir": os.Getenv("RSRS_DATA_DIR"), "session": map[string]any{"addr": "", "has_token": false}}}})
}

func TestRuntimeSDKSpaceCreateUnknownAndRemoteCustody(t *testing.T) {
	for _, remote := range []bool{false, true} {
		t.Run(fmt.Sprint("remote=", remote), func(t *testing.T) {
			writes := 0
			manager := v1TestManager(t, []string{"openapi.v1"}, func(w http.ResponseWriter, r *http.Request) {
				if r.URL.Path == "/api/rpc" {
					writeSpaceStatus(t, w, r, remote)
					return
				}
				writes++
				w.WriteHeader(409)
				io.WriteString(w, `{"status":409,"title":"Inspect space state","code":"space_create_outcome_unknown"}`)
			})
			_, handled, err := manager.executeV1Operation(context.Background(), "space_create", "id", json.RawMessage(`{"name":"new"}`))
			if remote {
				if handled || err != nil || writes != 0 {
					t.Fatalf("remote custody reached V1 create: %v %v writes=%d", handled, err, writes)
				}
			} else {
				var unknown *unconfirmedError
				var problem *RuntimeHTTPError
				if !handled || !errors.As(err, &unknown) || !errors.As(err, &problem) || problem.Code != "space_create_outcome_unknown" || writes != 1 {
					t.Fatalf("unknown outcome was lost: %v", err)
				}
			}
		})
	}
}

func TestRuntimeSDKRecoveryDistinguishesTerminalAndTransportFailures(t *testing.T) {
	for _, tc := range []struct {
		name, state      string
		success, unknown bool
	}{
		{"model_cancel", "cancelled", true, false}, {"sync", "failed", false, false}, {"sync", "cancelled", false, false}, {"sync", "running", false, true},
	} {
		t.Run(tc.name+tc.state, func(t *testing.T) {
			manager := v1TestManager(t, []string{"sse.operations.v1"}, func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "text/event-stream")
				event := tc.state
				if event == "running" {
					event = "progress"
				}
				io.WriteString(w, operationFrame(event, tc.state, "op"))
			})
			_, err := manager.recoverV1Operation(context.Background(), Operation{ID: "id", Name: tc.name, RuntimeID: "op", RuntimeV1: true})
			var unknown *unconfirmedError
			if (err == nil) != tc.success || errors.As(err, &unknown) != tc.unknown {
				t.Fatalf("result classification: %v", err)
			}
		})
	}
}

func TestRuntimeSDKReadHandlersPreserveLegacyOnlyFields(t *testing.T) {
	var light, syncStatus, catalog bool
	manager := v1TestManager(t, []string{"openapi.v1", "model.catalog.v2"}, func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/v1/status/light":
			light = true
			io.WriteString(w, `{"memory_count":42,"diary_count":3,"indexed_count":40,"space_id":"main"}`)
		case "/v1/sync/status":
			syncStatus = true
			io.WriteString(w, `{"state":"idle","pending_outbox":8}`)
		case "/v1/models/catalog":
			catalog = true
			io.WriteString(w, `{"active_model_id":"m3","models":[]}`)
		case "/api/rpc":
			var request map[string]any
			if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
				t.Error(err)
			}
			envelope := map[string]any{"command": "status", "status": "ok", "summary": map[string]any{
				"local_alive": 1, "session": map[string]any{"has_token": true}, "sync_live": map[string]any{"conflicts": 2},
			}}
			if request["method"] == "model.catalog" {
				envelope = map[string]any{"status": "ok", "details": map[string]any{"active": map[string]string{"model": "m3", "engine": "cpu", "variant": "quantized"}}}
			}
			json.NewEncoder(w).Encode(map[string]any{"v": 1, "id": request["id"], "operation_id": request["id"], "scope": request["scope"], "ok": true, "envelope": envelope})
		default:
			t.Errorf("unexpected route: %s", r.URL.Path)
			http.NotFound(w, r)
		}
	})
	raw, err := manager.RuntimeStatus(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	var envelope struct {
		Summary struct {
			LocalAlive int `json:"local_alive"`
			Session    struct {
				HasToken bool `json:"has_token"`
			} `json:"session"`
			Sync struct {
				Pending   int `json:"pending"`
				Conflicts int `json:"conflicts"`
			} `json:"sync_live"`
		} `json:"summary"`
	}
	if err := json.Unmarshal(raw, &envelope); err != nil {
		t.Fatal(err)
	}
	if envelope.Summary.LocalAlive != 42 || !envelope.Summary.Session.HasToken || envelope.Summary.Sync.Pending != 8 || envelope.Summary.Sync.Conflicts != 2 {
		t.Fatalf("status fields were lost: %s", raw)
	}
	raw, err = manager.ModelCatalogRPC(context.Background())
	if err != nil || !strings.Contains(string(raw), `"engine":"cpu"`) || !strings.Contains(string(raw), `"active_model_id":"m3"`) {
		t.Fatalf("catalog=%s err=%v", raw, err)
	}
	if !light || !syncStatus || !catalog {
		t.Fatal("SDK read handler was skipped")
	}
}

func TestRuntimeSDKSnapshotWithoutSSEAndLegacyModelControl(t *testing.T) {
	manager := v1TestManager(t, []string{"openapi.v1"}, func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/v1/operations/legacy" {
			http.NotFound(w, r)
			return
		}
		io.WriteString(w, `{"operation_id":"op","kind":"model_switch","state":"completed"}`)
	})
	if _, err := manager.RuntimeOperation(context.Background(), "op", false); err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"model_progress", "model_cancel"} {
		if _, handled, err := manager.executeV1Operation(context.Background(), name, "id", json.RawMessage(`{"id":"legacy"}`)); err != nil || handled {
			t.Fatalf("legacy model control: %s handled=%v err=%v", name, handled, err)
		}
	}
	if _, err := manager.RuntimeOperation(context.Background(), "op", true); err == nil {
		t.Fatal("cancellation was not capability gated")
	}
}

func TestRuntimeSDKRetainsUnrepresentedOperations(t *testing.T) {
	manager := &Manager{}
	for _, name := range []string{"diary_search", "session_import_apply", "memory_tree", "memory_chain", "memory_share", "grant_create", "model_uninstall", "account_use", "memory_export"} {
		if _, handled, err := manager.executeV1Operation(context.Background(), name, "id", nil); err != nil || handled {
			t.Fatalf("%s entered V1 dispatcher: %v", name, err)
		}
	}
	for _, name := range []string{"space_use", "space_remove"} {
		if _, handled, err := manager.executeV1Operation(context.Background(), name, "id", json.RawMessage(`{"name":"work"}`)); handled || err != nil {
			t.Fatalf("historical space names must not select a same-name V1 space: %v", err)
		}
	}
}

func TestRuntimeSDKGraniteKeepsRegistrationAndOmitsVariant(t *testing.T) {
	configured := false
	manager := v1TestManager(t, []string{"openapi.v1", "sse.operations.v1"}, func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/api/rpc":
			var request struct {
				Method string   `json:"method"`
				Args   []string `json:"args"`
			}
			if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
				t.Error(err)
			}
			if request.Method != "model.configure" || len(request.Args) != 1 || !strings.Contains(request.Args[0], "granite-97m-r2") {
				t.Errorf("invalid profile registration: %+v", request)
			}
			configured = true
			io.WriteString(w, `{"ok":true,"envelope":{"status":"ok"}}`)
		case "/v1/models/switch":
			if !configured {
				t.Error("switch preceded profile registration")
			}
			var request map[string]any
			if err := json.NewDecoder(r.Body).Decode(&request); err != nil {
				t.Error(err)
			}
			if _, exists := request["variant"]; exists {
				t.Error("Granite variant must be omitted")
			}
			w.WriteHeader(202)
			io.WriteString(w, `{"operation_id":"op","status":"accepted"}`)
		default:
			t.Errorf("unexpected route: %s", r.URL.Path)
			http.NotFound(w, r)
		}
	})
	if _, err := manager.StartModelSwitch(context.Background(), "granite-97m-r2", "cpu", "standard"); err != nil {
		t.Fatal(err)
	}
}

func TestRuntimeSDKBackupReadFailureDoesNotReplaceExistingFile(t *testing.T) {
	manager := v1TestManager(t, []string{"backups.v1"}, func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Length", "100")
		io.WriteString(w, "truncated")
	})
	path := filepath.Join(t.TempDir(), "existing.rsrs-backup")
	if err := os.WriteFile(path, []byte("previous archive"), 0600); err != nil {
		t.Fatal(err)
	}
	client, _, err := manager.runtimeSDK(context.Background(), "backups.v1")
	if err != nil {
		t.Fatal(err)
	}
	if _, err := manager.backupV1(context.Background(), client, path, false); err == nil {
		t.Fatal("truncated archive was accepted")
	}
	raw, err := os.ReadFile(path)
	if err != nil || string(raw) != "previous archive" {
		t.Fatalf("existing backup changed: %q %v", raw, err)
	}
	files, err := os.ReadDir(filepath.Dir(path))
	if err != nil || len(files) != 1 {
		t.Fatalf("temporary archive was not cleaned: %v %v", files, err)
	}
}
