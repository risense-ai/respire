package host

import (
	"archive/tar"
	"bytes"
	"compress/gzip"
	"context"
	"encoding/json"
	"errors"
	"io"
	"io/fs"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"respire-client2/internal/i18n"
	"strings"
	"testing"
)

func TestPublishRuntimeStagingAndPromotion(t *testing.T) {
	buildPayload(t, "1.2.3")
	payload, err := readBootstrapPayload()
	if err != nil {
		t.Fatal(err)
	}
	meta := payload.Metadata

	t.Run("payload copy verifies and publishes", func(t *testing.T) {
		m := newInstallTestManager(t)
		if err = m.publishRuntime(meta, stageBootstrapRuntime(payload.Dir), true); err != nil {
			t.Fatalf("publish failed: %v", err)
		}
		if err = verifyRuntime(m.runtimeDir(meta.RuntimeVersion), meta.BinaryHash); err != nil {
			t.Fatalf("published dir must verify: %v", err)
		}
		if _, err = os.Stat(filepath.Join(m.runtimeDir(meta.RuntimeVersion), "core-notices")); !errors.Is(err, os.ErrNotExist) {
			t.Fatal("managed Runtime must omit notices")
		}
		if _, err = os.Stat(filepath.Join(m.Root, "runtime.json")); !errors.Is(err, os.ErrNotExist) {
			t.Fatal("publishRuntime must not write the version pointer")
		}
	})
	t.Run("existing version dir rejects unauthenticated files without deleting them", func(t *testing.T) {
		m := newInstallTestManager(t)
		dest := m.runtimeDir(meta.RuntimeVersion)
		if err = m.publishRuntime(meta, stageBootstrapRuntime(payload.Dir), true); err != nil {
			t.Fatal(err)
		}
		if err = copyTree(filepath.Join(payload.Dir, bootstrapRuntimeDir, "core-notices"), filepath.Join(dest, "core-notices")); err != nil {
			t.Fatal(err)
		}
		if err = m.publishRuntime(meta, stageBootstrapRuntime(payload.Dir), true); err != nil {
			t.Fatal(err)
		}
		if _, err = os.Stat(filepath.Join(dest, "core-notices", "NOTICES.txt")); err != nil {
			t.Fatal("reusing an installed version must preserve its notices")
		}
		stray := filepath.Join(dest, "stray-user-file")
		if err = os.WriteFile(stray, []byte("user data"), 0600); err != nil {
			t.Fatal(err)
		}
		if err = m.publishRuntime(meta, stageBootstrapRuntime(payload.Dir), true); err == nil {
			t.Fatal("unauthenticated files must prevent reuse")
		}
		if _, err = os.Stat(stray); err != nil {
			t.Fatal("existing version dir must never be merged or cleaned")
		}
	})
	t.Run("interrupted fill publishes nothing", func(t *testing.T) {
		m := newInstallTestManager(t)
		err = m.publishRuntime(meta, func(stage string) error {
			return errors.New("interrupted mid-copy")
		}, true)
		if err == nil {
			t.Fatal("interrupted fill must fail")
		}
		if _, err = os.Stat(m.runtimeDir(meta.RuntimeVersion)); !errors.Is(err, os.ErrNotExist) {
			t.Fatal("interrupted install must not publish the version dir")
		}
		entries, _ := os.ReadDir(filepath.Dir(m.runtimeDir(meta.RuntimeVersion)))
		for _, e := range entries {
			if strings.HasPrefix(e.Name(), ".stage-") {
				t.Fatalf("staging leftovers after interruption: %s", e.Name())
			}
		}
	})
	t.Run("tampered tree fails verification and publishes nothing", func(t *testing.T) {
		m := newInstallTestManager(t)
		tampered := t.TempDir()
		if err = copyTree(filepath.Join(payload.Dir, bootstrapRuntimeDir), tampered); err != nil {
			t.Fatal(err)
		}
		notices := filepath.Join(tampered, "core-notices", "NOTICES.txt")
		if err = os.WriteFile(notices, []byte("tampered\n"), 0600); err != nil {
			t.Fatal(err)
		}
		// The tampered tree replaces the payload runtime dir wholesale.
		err = m.publishRuntime(meta, func(stage string) error { return copyTree(tampered, stage) }, true)
		if err == nil || !strings.Contains(err.Error(), "checksum mismatch") {
			t.Fatalf("tampered tree must fail verification, got %v", err)
		}
		if _, err = os.Stat(m.runtimeDir(meta.RuntimeVersion)); !errors.Is(err, os.ErrNotExist) {
			t.Fatal("failed verification must not publish")
		}
	})
	t.Run("installation lock excludes another publisher", func(t *testing.T) {
		m := newInstallTestManager(t)
		ctx, cancel := context.WithCancel(context.Background())
		lock, err := m.lockRuntime(ctx)
		if err != nil {
			cancel()
			t.Fatal(err)
		}
		other, err := os.OpenFile(filepath.Join(filepath.Dir(m.runtimeDir(meta.RuntimeVersion)), ".install.lock"), os.O_RDWR, 0600)
		if err != nil {
			lock.Close()
			cancel()
			t.Fatal(err)
		}
		if err = tryRuntimeLock(other); err == nil {
			other.Close()
			lock.Close()
			cancel()
			t.Fatal("install lock must exclude another lock")
		}
		other.Close()
		lock.Close()
		cancel()
	})
}

// fakeGH rewrites every request to the test server so the production fetch
// code runs unmodified against fixture release metadata and assets.
type fakeGH struct{ server string }

func (f fakeGH) RoundTrip(req *http.Request) (*http.Response, error) {
	req.URL.Scheme = "http"
	req.URL.Host = strings.TrimPrefix(f.server, "http://")
	return http.DefaultTransport.RoundTrip(req)
}

func buildTarGz(t *testing.T, dir string) []byte {
	t.Helper()
	var buf bytes.Buffer
	gz := gzip.NewWriter(&buf)
	tw := tar.NewWriter(gz)
	err := filepath.WalkDir(dir, func(path string, d fs.DirEntry, err error) error {
		if err != nil {
			return err
		}
		rel, err := filepath.Rel(dir, path)
		if err != nil {
			return err
		}
		if rel == "." {
			return nil
		}
		rel = filepath.ToSlash(rel)
		info, err := d.Info()
		if err != nil {
			return err
		}
		if d.IsDir() {
			return tw.WriteHeader(&tar.Header{Name: rel, Typeflag: tar.TypeDir, Mode: int64(info.Mode().Perm())})
		}
		if err = tw.WriteHeader(&tar.Header{Name: rel, Typeflag: tar.TypeReg, Mode: int64(info.Mode().Perm()), Size: info.Size()}); err != nil {
			return err
		}
		f, err := os.Open(path)
		if err != nil {
			return err
		}
		defer f.Close()
		_, err = io.Copy(tw, f)
		return err
	})
	if err != nil {
		t.Fatal(err)
	}
	if err = tw.Close(); err != nil {
		t.Fatal(err)
	}
	if err = gz.Close(); err != nil {
		t.Fatal(err)
	}
	return buf.Bytes()
}

func TestRuntimeStartInstallationState(t *testing.T) {
	t.Run("absent Runtime and CLI allow first install", func(t *testing.T) {
		m := newInstallTestManager(t)
		if err := os.WriteFile(filepath.Join(m.DataRoot, "runtime.json"), []byte("{broken legacy receipt"), 0600); err != nil {
			t.Fatal(err)
		}
		if err := m.Start(); !errors.Is(err, ErrRuntimeNotInstalled) {
			t.Fatalf("an obsolete receipt must not override missing installation: %v", err)
		}
	})
	t.Run("missing Runtime does not inspect an external CLI", func(t *testing.T) {
		m := newInstallTestManager(t)
		path := filepath.Join(m.DataRoot, "bin", cliBinary())
		if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, []byte("invalid executable"), 0700); err != nil {
			t.Fatal(err)
		}
		if err := m.Start(); !errors.Is(err, ErrRuntimeNotInstalled) {
			t.Fatalf("the CLI image must not affect Runtime installation state: %v", err)
		}
	})
	t.Run("corrupt selection is not missing", func(t *testing.T) {
		m := newInstallTestManager(t)
		if err := os.MkdirAll(filepath.Dir(m.runtimeManifest()), 0700); err != nil {
			t.Fatal(err)
		}
		if err := writeJSON(m.runtimeManifest(), map[string]any{"version": "invalid"}); err != nil {
			t.Fatal(err)
		}
		if err := m.Start(); err == nil || errors.Is(err, ErrRuntimeNotInstalled) {
			t.Fatalf("invalid selection must retain its failure: %v", err)
		}
	})
	t.Run("selected Runtime with missing files is not a fresh install", func(t *testing.T) {
		m := newInstallTestManager(t)
		if err := os.MkdirAll(filepath.Dir(m.runtimeManifest()), 0700); err != nil {
			t.Fatal(err)
		}
		if err := writeJSON(m.runtimeManifest(), InstalledRuntime{Version: "1.0.21", SHA256: strings.Repeat("a", 64)}); err != nil {
			t.Fatal(err)
		}
		if err := m.Start(); !errors.Is(err, os.ErrNotExist) || errors.Is(err, ErrRuntimeNotInstalled) {
			t.Fatalf("missing selected files must retain the installation failure: %v", err)
		}
	})
	t.Run("running owned Runtime reports missing or corrupt selection", func(t *testing.T) {
		for _, corrupt := range []bool{false, true} {
			name := "missing"
			if corrupt {
				name = "corrupt"
			}
			t.Run(name, func(t *testing.T) {
				m := newInstallTestManager(t)
				if corrupt {
					if err := os.MkdirAll(filepath.Dir(m.runtimeManifest()), 0700); err != nil {
						t.Fatal(err)
					}
					if err := writeJSON(m.runtimeManifest(), map[string]any{"version": "invalid"}); err != nil {
						t.Fatal(err)
					}
				}
				m.Endpoint = newFakeRuntimeServer(t, filepath.Join(m.runtimeDir("1.0.21"), runtimeBinary()), "").URL
				err := m.Start()
				if err == nil || !strings.Contains(err.Error(), i18n.Message2379289351d6f0ce) {
					t.Fatalf("owned Runtime must report a %s selection: %v", name, err)
				}
				if !corrupt && !errors.Is(err, os.ErrNotExist) {
					t.Fatalf("missing selection must retain its cause: %v", err)
				}
			})
		}
	})
	t.Run("running external Runtime remains usable", func(t *testing.T) {
		m := newInstallTestManager(t)
		if err := os.MkdirAll(filepath.Dir(m.runtimeManifest()), 0700); err != nil {
			t.Fatal(err)
		}
		if err := writeJSON(m.runtimeManifest(), map[string]any{"version": "invalid"}); err != nil {
			t.Fatal(err)
		}
		m.Endpoint = newFakeRuntimeServer(t, "/external/rsrs-runtime", "").URL
		if err := m.Start(); err != nil {
			t.Fatalf("running external Runtime must not require a local selection: %v", err)
		}
	})
	t.Run("unlock failure is not a fresh install", func(t *testing.T) {
		m := newInstallTestManager(t)
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			_, _ = io.WriteString(w, `{"server":"respire","v":1,"pid":4242,"credential_state":"required"}`)
		}))
		defer srv.Close()
		m.Endpoint = srv.URL
		if err := m.Start(); err == nil || errors.Is(err, ErrRuntimeNotInstalled) {
			t.Fatalf("unlock host failure must not authorize install: %v", err)
		}
	})
}

func TestBootstrapPayloadRejectsInvalidLocalComponents(t *testing.T) {
	target, err := runtimeTarget()
	if err != nil {
		t.Fatal(err)
	}
	metadataPath := func(dir string) string {
		return filepath.Join(dir, "runtime-build-"+target+".json")
	}
	damageMetadata := func(change func(map[string]any)) func(string) error {
		return func(dir string) error {
			path := metadataPath(dir)
			data, err := os.ReadFile(path)
			if err != nil {
				return err
			}
			var metadata map[string]any
			if err = json.Unmarshal(data, &metadata); err != nil {
				return err
			}
			change(metadata)
			data, err = json.Marshal(metadata)
			if err != nil {
				return err
			}
			return os.WriteFile(path, data, 0600)
		}
	}
	for _, tc := range []struct {
		name    string
		damage  func(string) error
		want    string
		missing bool
	}{
		{"missing manifest", func(dir string) error { return os.Remove(filepath.Join(dir, bootstrapManifestFile)) }, bootstrapManifestFile, true},
		{"missing Runtime metadata", func(dir string) error { return os.Remove(metadataPath(dir)) }, "embedded Runtime metadata", true},
		{"invalid Runtime metadata", func(dir string) error { return os.WriteFile(metadataPath(dir), []byte("{"), 0600) }, "embedded Runtime metadata", false},
		{"mismatched Runtime version", damageMetadata(func(meta map[string]any) { meta["runtime_version"] = "1.0.22" }), "metadata does not match bootstrap manifest", false},
		{"incomplete Runtime proof", damageMetadata(func(meta map[string]any) { delete(meta, "files") }), i18n.Message89a5f726afd59f27, false},
		{"invalid manifest", func(dir string) error {
			return os.WriteFile(filepath.Join(dir, bootstrapManifestFile), []byte("{"), 0600)
		}, "unexpected EOF", false},
		{"invalid target", func(dir string) error {
			path := filepath.Join(dir, bootstrapManifestFile)
			data, err := os.ReadFile(path)
			if err != nil {
				return err
			}
			target, err := runtimeTarget()
			if err != nil {
				return err
			}
			return os.WriteFile(path, bytes.Replace(data, []byte(target), []byte("invalid-target"), 1), 0600)
		}, "embedded payload targets invalid-target, not", false},
		{"foreign target with invalid channel", func(dir string) error {
			path := filepath.Join(dir, bootstrapManifestFile)
			data, err := os.ReadFile(path)
			if err != nil {
				return err
			}
			target, err := runtimeTarget()
			if err != nil {
				return err
			}
			foreign := "x86_64-unknown-linux-gnu"
			if target == foreign {
				foreign = "aarch64-apple-darwin"
			}
			data = bytes.Replace(data, []byte(target), []byte(foreign), 1)
			data = bytes.Replace(data, []byte(`"channel":"dev"`), []byte(`"channel":"invalid"`), 1)
			return os.WriteFile(path, data, 0600)
		}, "embedded payload targets", false},
		{"missing CLI", func(dir string) error { return os.Remove(filepath.Join(dir, cliBinary())) }, cliBinary(), true},
		{"tampered CLI", func(dir string) error { return os.WriteFile(filepath.Join(dir, cliBinary()), []byte("tampered"), 0700) }, "embedded CLI hash mismatch", false},
		{"missing Runtime", func(dir string) error { return os.Remove(filepath.Join(dir, bootstrapRuntimeDir, runtimeBinary())) }, "embedded Runtime verification failed", true},
		{"tampered Runtime", func(dir string) error {
			return os.WriteFile(filepath.Join(dir, bootstrapRuntimeDir, runtimeBinary()), []byte("tampered"), 0700)
		}, "embedded Runtime verification failed", false},
	} {
		t.Run(tc.name, func(t *testing.T) {
			dir := buildPayload(t, "1.2.3")
			if err := tc.damage(dir); err != nil {
				t.Fatal(err)
			}
			requests := make(chan struct{}, 1)
			server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				select {
				case requests <- struct{}{}:
				default:
				}
				http.Error(w, "unexpected release request", http.StatusServiceUnavailable)
			}))
			defer server.Close()
			m := newInstallTestManager(t)
			m.HTTPClient = &http.Client{Transport: fakeGH{server: server.URL}}
			_, err := readBootstrapPayload()
			if err == nil || !strings.Contains(err.Error(), tc.want) {
				t.Fatalf("invalid payload must expose %q: %v", tc.want, err)
			}
			if tc.missing && !errors.Is(err, os.ErrNotExist) {
				t.Fatalf("missing payload component must retain its cause: %v", err)
			}
			if status := m.BootstrapStatus(context.Background()); !strings.Contains(status.PayloadError, tc.want) {
				t.Fatalf("diagnostics must report the payload failure: %+v", status)
			}
			select {
			case <-requests:
				t.Fatal("invalid payload must not trigger online release discovery")
			default:
			}
		})
	}
}
