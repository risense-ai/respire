package desktop

import (
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"

	"respire-client2/internal/host"
)

func TestRuntimeOperationDesktopSSEAndCancellation(t *testing.T) {
	t.Setenv("RSRS_DATA_DIR", t.TempDir())
	t.Setenv("RSRS_RPC_TOKEN", "fixture-token")
	var cancelled bool
	upstream := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/v1/health" {
			io.WriteString(w, `{"status":"ready","instance_id":"instance","capabilities":["openapi.v1","sse.operations.v1"]}`)
			return
		}
		if r.Header.Get("Authorization") != "Bearer fixture-token" {
			t.Error("desktop did not authenticate the upstream request")
		}
		switch r.URL.Path {
		case "/v1/operations/op/events":
			w.Header().Set("Content-Type", "text/event-stream")
			io.WriteString(w, "event: progress\ndata: {\"event\":\"progress\",\"data\":{\"operation_id\":\"op\",\"kind\":\"model_switch\",\"state\":\"running\"}}\n\nevent: completed\ndata: {\"event\":\"completed\",\"data\":{\"operation_id\":\"op\",\"kind\":\"model_switch\",\"state\":\"completed\"}}\n\n")
		case "/v1/operations/op":
			cancelled = r.Method == "DELETE"
			w.WriteHeader(202)
			io.WriteString(w, `{"operation_id":"op","kind":"model_switch","state":"cancelled"}`)
		default:
			http.NotFound(w, r)
		}
	}))
	defer upstream.Close()
	manager, err := host.New()
	if err != nil {
		t.Fatal(err)
	}
	manager.Endpoint = upstream.URL
	d := &desktop{m: manager, closing: make(chan struct{})}
	w := httptest.NewRecorder()
	if !d.runtimeOperationRoute(w, httptest.NewRequest("GET", "/api/runtime-operation-events?id=op", nil)) {
		t.Fatal("stream route was not handled")
	}
	if w.Header().Get("Content-Type") != "text/event-stream" || !strings.Contains(w.Body.String(), "event: completed") || !strings.Contains(w.Body.String(), "event: progress") {
		t.Fatalf("invalid stream response: %s", w.Body)
	}
	for _, authorized := range []bool{false, true} {
		request := httptest.NewRequest("DELETE", "/api/runtime-operation?id=op", nil)
		if authorized {
			request.Header.Set("X-Respire-Client", "1")
		}
		w := httptest.NewRecorder()
		d.runtimeOperationRoute(w, request)
		if authorized != cancelled || !authorized && w.Code != http.StatusForbidden {
			t.Fatalf("cancellation authorization=%v cancelled=%v HTTP=%d", authorized, cancelled, w.Code)
		}
	}
}

func TestRuntimeOperationDesktopRejectsMissingIdentity(t *testing.T) {
	d := &desktop{}
	w := httptest.NewRecorder()
	d.runtimeOperationRoute(w, httptest.NewRequest("GET", "/api/runtime-operation-events", nil))
	if w.Code != http.StatusForbidden {
		t.Fatalf("missing ID returned HTTP %d", w.Code)
	}
}
