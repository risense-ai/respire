package desktop

import (
	"bufio"
	"context"
	"net/http"
	"net/http/httptest"
	"respire-client2/internal/host"
	"testing"
	"time"
)

func TestDownloadStreamBoundsAndShutdown(t *testing.T) {
	var streams downloadStreams
	var manager host.Manager
	closing := make(chan struct{})
	finish := make(chan struct{})
	defer close(finish)
	id := manager.StartDownload("test", func(context.Context) error { <-finish; return nil })
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		streams.serve(w, r.WithContext(context.Background()), &manager, closing)
	}))
	defer server.Close()
	defer func() {
		select {
		case <-closing:
		default:
			close(closing)
		}
	}()
	client := server.Client()
	client.Timeout = 3 * time.Second
	var responses []*http.Response
	for i := 0; i < 5; i++ {
		res, err := client.Get(server.URL + "?id=" + id)
		if err != nil {
			t.Fatal(err)
		}
		defer res.Body.Close()
		if i == 4 {
			if res.StatusCode != http.StatusTooManyRequests {
				t.Fatal(res.Status)
			}
			break
		}
		if res.StatusCode != 200 {
			t.Fatal(res.Status)
		}
		responses = append(responses, res)
	}
	close(closing)
	for _, res := range responses {
		scanner := bufio.NewScanner(res.Body)
		for scanner.Scan() {
		}
		if scanner.Err() != nil {
			t.Fatal(scanner.Err())
		}
	}
	if streams.active.Load() != 0 {
		t.Fatal("subscriber leaked")
	}
}

func TestDownloadStreamReleasesSubscriberOnDisconnect(t *testing.T) {
	var streams downloadStreams
	var manager host.Manager
	finish := make(chan struct{})
	defer close(finish)
	id := manager.StartDownload("test", func(context.Context) error { <-finish; return nil })
	done := make(chan struct{})
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		defer close(done)
		streams.serve(w, r, &manager, nil)
	}))
	defer server.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, server.URL+"?id="+id, nil)
	if err != nil {
		t.Fatal(err)
	}
	res, err := server.Client().Do(req)
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	if res.StatusCode != http.StatusOK || streams.active.Load() != 1 {
		t.Fatalf("expected a live subscription, got %s with %d active", res.Status, streams.active.Load())
	}

	cancel()
	select {
	case <-done:
	case <-time.After(3 * time.Second):
		t.Fatal("handler did not exit after client disconnect")
	}
	if streams.active.Load() != 0 {
		t.Fatal("subscriber leaked")
	}
}
