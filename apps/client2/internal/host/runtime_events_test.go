package host

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"

	runtimesdk "github.com/risense-ai/respire/packages/runtime-sdk-go"
)

func operationFrame(event, state, id string) string {
	return fmt.Sprintf("id: 2\r\nevent: %s\r\ndata: {\"event\":%q,\r\ndata: \"data\":{\"operation_id\":%q,\"kind\":\"model_switch\",\"state\":%q,\"progress_percent\":40}}\r\n\r\n", event, event, id, state)
}

func v1TestManager(t *testing.T, capabilities []string, handler http.HandlerFunc) *Manager {
	t.Helper()
	t.Setenv("RSRS_RPC_TOKEN", "fixture-token")
	dataRoot := t.TempDir()
	t.Setenv("RSRS_DATA_DIR", dataRoot)
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/v1/health" {
			json.NewEncoder(w).Encode(map[string]any{"status": "ready", "instance_id": "instance", "capabilities": capabilities})
			return
		}
		if r.URL.Path == "/api/health" {
			io.WriteString(w, `{"server":"respire","v":1,"pid":42,"account_id":"account","instance_id":"instance","generation":7}`)
			return
		}
		if strings.HasPrefix(r.URL.Path, "/v1/") && r.Header.Get("Authorization") != "Bearer fixture-token" {
			t.Error("SDK request did not carry the installation bearer")
			w.WriteHeader(http.StatusUnauthorized)
			return
		}
		handler(w, r)
	}))
	t.Cleanup(server.Close)
	return &Manager{Endpoint: server.URL, DataRoot: dataRoot, Root: t.TempDir(), localClient: server.Client()}
}

func TestRuntimeSSEParsesProgressAndAllTerminalStates(t *testing.T) {
	for _, terminal := range []string{"completed", "failed", "cancelled"} {
		t.Run(terminal, func(t *testing.T) {
			manager := v1TestManager(t, []string{"sse.operations.v1"}, func(w http.ResponseWriter, r *http.Request) {
				if r.Method != "GET" || r.URL.Path != "/v1/operations/op/events" {
					t.Errorf("unexpected request: %s %s", r.Method, r.URL.Path)
				}
				w.Header().Set("Content-Type", "text/event-stream; charset=utf-8")
				io.WriteString(w, ": keepalive\r\n\r\n"+operationFrame("progress", "running", "op")+operationFrame(terminal, terminal, "op"))
			})
			var events []string
			err := manager.StreamRuntimeOperation(context.Background(), "op", func(event runtimesdk.OperationEvent) error {
				events = append(events, string(event.Event))
				if event.Data.ProgressPercent == nil || *event.Data.ProgressPercent != 40 {
					t.Error("progress percentage was lost")
				}
				return nil
			})
			if err != nil || strings.Join(events, ",") != "progress,"+terminal {
				t.Fatalf("events=%v, error=%v", events, err)
			}
		})
	}
}

func TestRuntimeSSERejectsInvalidOrIncompleteEvents(t *testing.T) {
	for name, body := range map[string]string{
		"wrong identity": operationFrame("completed", "completed", "other"),
		"wrong state":    operationFrame("completed", "running", "op"),
		"invalid state":  operationFrame("progress", "unknown", "op"),
		"invalid json":   "event: progress\ndata: {\n\n",
		"early EOF":      operationFrame("progress", "running", "op"),
		"partial frame":  "event: completed\ndata: {}",
	} {
		t.Run(name, func(t *testing.T) {
			if err := parseOperationEvents(strings.NewReader(body), "op", func(runtimesdk.OperationEvent) error { return nil }); err == nil {
				t.Fatal("invalid stream was accepted")
			}
		})
	}
}

func TestRuntimeSSEDisconnectCancelsRequest(t *testing.T) {
	closed := make(chan struct{})
	manager := v1TestManager(t, []string{"sse.operations.v1"}, func(w http.ResponseWriter, r *http.Request) {
		time.Sleep(150 * time.Millisecond)
		w.Header().Set("Content-Type", "text/event-stream")
		io.WriteString(w, operationFrame("progress", "running", "op"))
		w.(http.Flusher).Flush()
		<-r.Context().Done()
		close(closed)
	})
	manager.localClient.Timeout = 100 * time.Millisecond // The stream must not inherit this deadline.
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	err := manager.StreamRuntimeOperation(ctx, "op", func(runtimesdk.OperationEvent) error { cancel(); return nil })
	if err == nil {
		t.Fatal("cancelled request returned success")
	}
	select {
	case <-closed:
	case <-time.After(time.Second):
		t.Fatal("upstream stream was left open")
	}
}

func TestRuntimeOperationWaitSurfacesFailureAndCancellation(t *testing.T) {
	for _, terminal := range []string{"failed", "cancelled"} {
		t.Run(terminal, func(t *testing.T) {
			manager := v1TestManager(t, []string{"sse.operations.v1"}, func(w http.ResponseWriter, r *http.Request) {
				w.Header().Set("Content-Type", "text/event-stream")
				io.WriteString(w, operationFrame(terminal, terminal, "op"))
			})
			err := manager.waitRuntimeOperation(context.Background(), runtimesdk.OperationAccepted{OperationId: "op"}, true, nil)
			if err == nil || terminal == "cancelled" && !errors.Is(err, context.Canceled) {
				t.Fatalf("terminal failure was lost: %v", err)
			}
		})
	}
}
