package host

import (
	"context"
	"errors"
	"io/fs"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
	"runtime"
	"strings"
	"syscall"
	"testing"
	"time"
)

func standaloneTestManager(t *testing.T) *Manager {
	t.Helper()
	root, err := canonicalSessionDirectory(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("RSRS_DATA_DIR", root)
	t.Setenv("RSRS_RPC_PORT", "1")
	m, err := New()
	if err != nil {
		t.Fatal(err)
	}
	return m
}

func standaloneTestSelection(t *testing.T, m *Manager) InstalledRuntime {
	t.Helper()
	buildPayload(t, "1.2.3")
	payload, err := readBootstrapPayload()
	if err != nil {
		t.Fatal(err)
	}
	if err := m.publishRuntime(payload.Metadata, stageBootstrapRuntime(payload.Dir), false); err != nil {
		t.Fatal(err)
	}
	info := InstalledRuntime{Version: payload.Metadata.RuntimeVersion, SHA256: payload.Metadata.BinaryHash}
	if err := m.selectRuntime(info); err != nil {
		t.Fatal(err)
	}
	if err := m.verifyRuntimeSelection(info); err != nil {
		t.Fatal(err)
	}
	return info
}

func standaloneTestSnapshot(t *testing.T, root string) map[string]string {
	t.Helper()
	files := make(map[string]string)
	err := filepath.WalkDir(root, func(path string, entry fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		if entry.IsDir() {
			files[path] = "directory"
			return nil
		}
		hash, err := fileHash(path)
		files[path] = hash
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	return files
}

func TestStandaloneColdStartWithUnsupportedStartupControl(t *testing.T) {
	m := standaloneTestManager(t)
	info := standaloneTestSelection(t, m)
	// A verifiable test image is not a serving Runtime. No authority is injected.
	before := standaloneTestSnapshot(t, m.DataRoot)
	refused := syscall.ECONNREFUSED
	if runtime.GOOS == "windows" {
		refused = syscall.Errno(10061) // WSAECONNREFUSED, not the Unix compatibility constant.
	}
	requests := 0
	m.localClient.Transport = cliMetadataTransport(func(r *http.Request) (*http.Response, error) {
		requests++
		if r.Method != http.MethodGet || r.URL.String() != m.Endpoint+"/api/health" {
			t.Fatalf("unexpected request: %s %s", r.Method, r.URL)
		}
		if deadline, ok := r.Context().Deadline(); !ok || time.Until(deadline) > 3*time.Second {
			t.Fatal("startup health probe has no bounded deadline")
		}
		return nil, &net.OpError{Op: "dial", Net: "tcp", Err: refused}
	})
	for _, tc := range []struct {
		name   string
		start  func(context.Context) error
		probes int
	}{
		{"prepare", m.PrepareRuntimeForStartup, 1},
		{"explicit start", func(context.Context) error { return m.Start() }, 1},
		{"detached", func(ctx context.Context) error {
			h, err := m.startDetached(ctx, info)
			if h != (HealthInfo{}) {
				t.Fatalf("failed startup returned health: %+v", h)
			}
			return err
		}, 0},
	} {
		t.Run(tc.name, func(t *testing.T) {
			requests = 0
			ctx, cancel := context.WithTimeout(t.Context(), 2*time.Second)
			defer cancel()
			if err := tc.start(ctx); err == nil || !strings.Contains(err.Error(), "Runtime startup control:") {
				t.Fatalf("verified cold start must expose the unsupported private protocol: %v", err)
			}
			if requests != tc.probes || m.directRuntime != (HealthInfo{}) {
				t.Fatalf("startup advanced past admission: probes=%d, owner=%+v", requests, m.directRuntime)
			}
			if after := standaloneTestSnapshot(t, m.DataRoot); !reflect.DeepEqual(before, after) {
				t.Fatal("failed cold start changed installation, endpoint, profile, or logs")
			}
		})
	}
	t.Run("existing identity", func(t *testing.T) {
		profile := filepath.Join(m.DataRoot, "accounts", "fixture")
		if err := os.MkdirAll(profile, 0700); err != nil {
			t.Fatal(err)
		}
		// This binds one synthetic identity; it is not a native login/session fixture.
		if err := writeJSON(filepath.Join(profile, "session.json"), map[string]string{"user": "fixture-existing"}); err != nil {
			t.Fatal(err)
		}
		if err := writeJSON(filepath.Join(m.DataRoot, "client.json"), map[string]string{"data_dir": profile}); err != nil {
			t.Fatal(err)
		}
		identity, err := deviceProfileIdentity(profile)
		if err != nil || identity.Kind != "bound" || identity.AccountID == nil || *identity.AccountID != controlDigest([]byte("fixture-existing")) {
			t.Fatalf("fixture identity is not bound: %+v, %v", identity, err)
		}
		before := standaloneTestSnapshot(t, m.DataRoot)
		ctx, cancel := context.WithTimeout(t.Context(), 2*time.Second)
		defer cancel()
		requests = 0
		if err := m.PrepareRuntimeForStartup(ctx); err == nil || !strings.Contains(err.Error(), "Runtime startup control:") {
			t.Fatalf("existing profile concealed the unsupported private protocol: %v", err)
		}
		current, err := deviceProfileIdentity(profile)
		if err != nil || !reflect.DeepEqual(current, identity) || requests != 1 || m.directRuntime != (HealthInfo{}) {
			t.Fatalf("failed cold start changed the bounded identity or owner: %+v, %v", current, err)
		}
		if after := standaloneTestSnapshot(t, m.DataRoot); !reflect.DeepEqual(before, after) {
			t.Fatal("failed cold start changed existing profile or installation bytes")
		}
	})
}

func TestStandaloneColdStartVerifiesSelectionBeforeAuthority(t *testing.T) {
	m := standaloneTestManager(t)
	info := standaloneTestSelection(t, m)
	if err := os.WriteFile(filepath.Join(m.runtimeDir(info.Version), runtimeBinary()), []byte("tampered"), 0700); err != nil {
		t.Fatal(err)
	}
	before := standaloneTestSnapshot(t, m.DataRoot)
	m.localClient.Transport = cliMetadataTransport(func(r *http.Request) (*http.Response, error) {
		t.Fatalf("invalid detached selection reached HTTP: %s", r.URL)
		return nil, errors.New("unexpected HTTP request")
	})
	ctx, cancel := context.WithTimeout(t.Context(), 2*time.Second)
	defer cancel()
	want := m.verifyRuntimeSelection(info)
	if want == nil {
		t.Fatal("tampered fixture passed installation verification")
	}
	if _, err := m.startDetached(ctx, info); err == nil || err.Error() != want.Error() {
		t.Fatalf("tampered selection must preserve installation verification error %q, got %v", want, err)
	}
	if after := standaloneTestSnapshot(t, m.DataRoot); !reflect.DeepEqual(before, after) {
		t.Fatal("invalid selection changed installation state")
	}
}

func TestStandaloneMaintenanceWithoutHostAuthority(t *testing.T) {
	m := standaloneTestManager(t)
	before := standaloneTestSnapshot(t, m.DataRoot)
	m.localClient.Transport = cliMetadataTransport(func(r *http.Request) (*http.Response, error) {
		t.Fatalf("unadmitted maintenance reached HTTP: %s", r.URL)
		return nil, errors.New("unexpected HTTP request")
	})
	for _, tc := range []struct {
		name string
		run  func(context.Context) error
	}{
		{"uninstall", func(ctx context.Context) error {
			result, err := m.uninstallM3Locked(ctx)
			if result != nil {
				t.Fatalf("unadmitted removal returned a result: %s", result)
			}
			return err
		}},
		{"account", func(ctx context.Context) error {
			_, lock, err := m.beginOfflineRuntimeAccountMaintenance(ctx, m.DataRoot, "test-issuer", runtimeLoginIntent{
				OperationID: "25bf4cf1-9865-47fa-bf59-5147307883f9", User: "fixture", Server: "https://example.invalid",
			})
			if lock != nil {
				t.Fatal("unadmitted account operation received ownership")
			}
			return err
		}},
		{"settings", func(ctx context.Context) error { return m.SetWorkspaceMode(ctx, "readonly") }},
	} {
		t.Run(tc.name, func(t *testing.T) {
			ctx, cancel := context.WithTimeout(t.Context(), 2*time.Second)
			defer cancel()
			if err := tc.run(ctx); !errors.Is(err, errRuntimeHostAuthorityUnavailable) {
				t.Fatalf("maintenance did not expose missing production authority: %v", err)
			}
			if after := standaloneTestSnapshot(t, m.DataRoot); !reflect.DeepEqual(before, after) {
				t.Fatal("unadmitted maintenance changed disposable profile state")
			}
		})
	}
}
