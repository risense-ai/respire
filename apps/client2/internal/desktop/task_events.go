package desktop

import (
	"encoding/json"
	"fmt"
	"net/http"
	"respire-client2/internal/host"
	"sync/atomic"
	"time"
)

// Host-owned downloads also run offline, without a Runtime operation ID.
type downloadStreams struct{ active atomic.Int32 }

func (streams *downloadStreams) serve(w http.ResponseWriter, r *http.Request, m *host.Manager, closing <-chan struct{}) {
	id := r.URL.Query().Get("id")
	defer streams.active.Add(-1)
	if streams.active.Add(1) > 4 {
		w.WriteHeader(http.StatusTooManyRequests)
		return
	}
	deadline := time.Now().Add(30 * time.Second)
	controller := http.NewResponseController(w)
	if err := controller.SetWriteDeadline(deadline); err != nil {
		w.WriteHeader(http.StatusNotImplemented)
		return
	}
	defer controller.SetWriteDeadline(time.Time{})
	timeout := time.NewTimer(time.Until(deadline))
	defer timeout.Stop()
	for {
		select {
		case <-timeout.C:
			return
		case <-closing:
			return
		case <-r.Context().Done():
			return
		default:
		}
		task, changed, found := m.WatchTask(id)
		if !found {
			w.WriteHeader(http.StatusNotFound)
			return
		}
		w.Header().Set("Content-Type", "text/event-stream")
		data, err := json.Marshal(task)
		if err != nil {
			return
		}
		if _, err := fmt.Fprintf(w, "data: %s\n\n", data); err != nil {
			return
		}
		if err := controller.Flush(); err != nil {
			return
		}
		if !task.Busy {
			return
		}
		select {
		case <-r.Context().Done():
			return
		case <-closing:
			return
		case <-timeout.C:
			return
		case <-changed:
		}
	}
}
