package host

import (
	"context"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"testing"
)

func TestLiveAssetsUsesLocalBearerOnlyAfterDiscovery(t *testing.T) {
	const token = "synthetic-loopback-token"
	t.Setenv("RSRS_RPC_TOKEN", token)
	health := HealthInfo{InstanceID: "synthetic-instance", PID: 42}
	reads := 0
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		switch r.URL.Path {
		case "/v1/health":
			if r.Header.Get("Authorization") != "" {
				t.Error("discovery received a credential")
			}
			_, _ = w.Write([]byte(`{"status":"ready","instance_id":"synthetic-instance","pid":42,"capabilities":["model.active_assets"]}`))
		case "/api/health":
			_, _ = w.Write([]byte(`{"server":"respire","v":1,"instance_id":"synthetic-instance","pid":42}`))
		case "/v1/models/active-assets":
			reads++
			if r.Header.Get("Authorization") != "Bearer "+token {
				w.WriteHeader(http.StatusUnauthorized)
				return
			}
			_, _ = w.Write([]byte(`{"active_model_id":"m3","active_paths":[]}`))
		default:
			http.NotFound(w, r)
		}
	}))
	defer server.Close()
	m := &Manager{Endpoint: server.URL, localClient: server.Client(), DataRoot: t.TempDir()}
	target := filepath.Join(m.DataRoot, "model")
	if err := m.checkLiveM3Assets(context.Background(), health, target); err != errM3Active || reads != 1 {
		t.Fatalf("active model inspection: error %v, reads %d", err, reads)
	}
	health.InstanceID = "different-instance"
	if err := m.checkLiveM3Assets(context.Background(), health, target); err == nil || reads != 1 {
		t.Fatal("sent a protected request after the runtime changed")
	}
}

func TestAssetBearerRefusesReplacementRedirectAndMissingToken(t *testing.T) {
	for _, mode := range []string{"closed", "redirect", "scope", "missing"} {
		t.Run(mode, func(t *testing.T) {
			t.Setenv("RSRS_RPC_TOKEN", "synthetic")
			if mode == "missing" {
				t.Setenv("RSRS_RPC_TOKEN", "")
			}
			requests := make(chan string, 12)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				requests <- r.URL.Path
				switch r.URL.Path {
				case "/v1/health":
					if mode == "closed" {
						w.Header().Set("Connection", "close")
					}
					_, _ = w.Write([]byte(`{"status":"ready","instance_id":"i","pid":42,"capabilities":["model.active_assets"]}`))
				case "/api/health":
					if mode == "scope" {
						_, _ = w.Write([]byte(`{"server":"respire","v":1,"instance_id":"i","pid":42,"generation":2}`))
						return
					}
					_, _ = w.Write([]byte(`{"server":"respire","v":1,"instance_id":"i","pid":42}`))
				case "/v1/models/active-assets":
					http.Redirect(w, r, "/replacement", http.StatusTemporaryRedirect)
				default:
					t.Error("followed redirect")
				}
			}))
			defer server.Close()
			root := t.TempDir()
			if err := os.Mkdir(filepath.Join(root, "runtime"), 0700); err != nil {
				t.Fatal(err)
			}
			m := &Manager{Endpoint: server.URL, DataRoot: root}
			if m.checkLiveM3Assets(context.Background(), HealthInfo{PID: 42, InstanceID: "i"}, "unused") == nil {
				t.Fatal("expected refusal")
			}
			close(requests)
			for p := range requests {
				if mode == "closed" && p != "/v1/health" || mode != "redirect" && p == "/v1/models/active-assets" {
					t.Fatalf("unexpected request %s", p)
				}
			}
			if _, err := os.Stat(filepath.Join(root, "runtime", "token")); !os.IsNotExist(err) {
				t.Fatal("inspection created token")
			}
		})
	}
}
