package desktop

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"respire-client2/internal/host"
	"time"

	runtimesdk "github.com/risense-ai/respire/packages/runtime-sdk-go"
)

func (d *desktop) runtimeOperationRoute(w http.ResponseWriter, r *http.Request) bool {
	if r.URL.Path != "/api/runtime-operation" && r.URL.Path != "/api/runtime-operation-events" {
		return false
	}
	id := r.URL.Query().Get("id")
	if id == "" || (r.Method != http.MethodGet && r.Method != http.MethodDelete) ||
		(r.Method == http.MethodDelete && (r.URL.Path != "/api/runtime-operation" || r.Header.Get("X-Respire-Client") != "1")) {
		http.Error(w, "Runtime operation request rejected", http.StatusForbidden)
		return true
	}
	if r.URL.Path == "/api/runtime-operation-events" {
		ctx, cancel := context.WithCancel(r.Context())
		defer cancel()
		go func() {
			select {
			case <-d.closing:
				cancel()
			case <-ctx.Done():
			}
		}()
		controller := http.NewResponseController(w)
		w.Header().Set("Content-Type", "text/event-stream")
		w.Header().Set("Cache-Control", "no-store")
		err := d.m.StreamRuntimeOperation(ctx, id, func(event runtimesdk.OperationEvent) error {
			data, err := json.Marshal(event)
			if err != nil {
				return err
			}
			if _, err := fmt.Fprintf(w, "event: %s\ndata: %s\n\n", event.Event, data); err != nil {
				return err
			}
			return controller.Flush()
		})
		if err != nil && ctx.Err() == nil {
			data, _ := json.Marshal(map[string]string{"error": err.Error()})
			fmt.Fprintf(w, "event: transport-error\ndata: %s\n\n", data)
			controller.Flush()
		}
		return true
	}
	ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
	defer cancel()
	result, err := d.m.RuntimeOperation(ctx, id, r.Method == http.MethodDelete)
	if err != nil {
		var runtimeError *host.RuntimeHTTPError
		if errors.As(err, &runtimeError) {
			w.WriteHeader(runtimeError.Status)
		}
		json.NewEncoder(w).Encode(map[string]any{"ok": false, "error": err.Error()})
	} else {
		json.NewEncoder(w).Encode(map[string]any{"ok": true, "result": result})
	}
	return true
}
