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
