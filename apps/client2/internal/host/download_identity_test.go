package host

import (
	"context"
	"testing"
	"time"
)

func TestDownloadReceiptSurvivesInterleavedTasks(t *testing.T) {
	m := new(Manager)
	start := func() (string, chan struct{}) {
		finish := make(chan struct{})
		id := m.StartDownload("download", func(context.Context) error { <-finish; return nil })
		if id == "" {
			t.Fatal("start failed")
		}
		return id, finish
	}
	complete := func(id string, finish chan struct{}) {
		_, changed, ok := m.WatchTask(id)
		if !ok {
			t.Fatal("missing active receipt")
		}
		close(finish)
		select {
		case <-changed:
		case <-time.After(time.Second):
			t.Fatal("missing completion")
		}
	}
	first, finish := start()
	complete(first, finish)
	for i := 0; i < 8; i++ {
		next, end := start()
		if next == first || m.CancelDownload(first, false) {
			t.Fatal("stale receipt controlled another task")
		}
		prior, _, ok := m.WatchTask(first)
		if !ok || prior.ID != first || prior.Busy {
			t.Fatal("new task replaced earlier receipt")
		}
		complete(next, end)
	}
	if _, _, ok := m.WatchTask(first); ok {
		t.Fatal("terminal retention exceeded")
	}
}
