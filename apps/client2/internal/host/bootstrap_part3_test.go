package host

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"respire-client2/internal/i18n"
	"strings"
	"sync/atomic"
	"testing"
)

func TestInstallRuntimeIndependently(t *testing.T) {
	t.Run("existing higher runtime version is not downgraded", func(t *testing.T) {
		payload, err := func() (bootstrapPayload, error) {
			buildPayload(t, "1.2.3")
			return readBootstrapPayload()
		}()
		if err != nil {
			t.Fatal(err)
		}
		m := newInstallTestManager(t)
		meta := payload.Metadata
		previous := InstalledRuntime{Version: "1.9.9", SHA256: meta.BinaryHash}
		if err = os.MkdirAll(m.runtimeDir(previous.Version), 0700); err != nil {
			t.Fatal(err)
		}
		if err = copyTree(filepath.Join(payload.Dir, bootstrapRuntimeDir), m.runtimeDir(previous.Version)); err != nil {
			t.Fatal(err)
		}
		if err = writeJSON(m.runtimeManifest(), previous); err != nil {
			t.Fatal(err)
		}
		if err = os.WriteFile(filepath.Join(payload.Dir, cliBinary()), []byte("tampered"), 0700); err != nil {
			t.Fatal(err)
		}
		stubCLIProbe(t, cliProbeFixture{tmpVersion: "1.2.3", destAbsent: true})
		// An existing installation checks online updates even if the unused
		// bundle is damaged. A failed check must preserve the receipt.
		releaseChecks := make(chan struct{}, 1)
		releaseServer := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			select {
			case releaseChecks <- struct{}{}:
			default:
			}
			http.Error(w, "offline fixture", http.StatusServiceUnavailable)
		}))
		defer releaseServer.Close()
		m.HTTPClient = &http.Client{Transport: fakeGH{server: releaseServer.URL}}
		if err = m.InstallRuntime(false, false); err == nil {
			t.Fatal("offline release check cannot succeed")
		}
		select {
		case <-releaseChecks:
		default:
			t.Fatal("existing installation must check online releases")
		}
		current, readErr := m.Installed()
		if readErr != nil || current.Version != "1.9.9" {
			t.Fatalf("higher installed version must stay selected: %+v err=%v", current, readErr)
		}
		if _, err = os.Stat(m.runtimeDir("1.0.21")); !errors.Is(err, os.ErrNotExist) {
			t.Fatal("the older payload version must not be published")
		}
		if _, err = os.Stat(filepath.Join(m.DataRoot, "bin", cliBinary())); !errors.Is(err, os.ErrNotExist) {
			t.Fatal("a failed online update must not replace the CLI from the bundle")
		}
	})
	t.Run("without payload the online path still installs", func(t *testing.T) {
		t.Setenv(bootstrapEnvOverride, filepath.Join(t.TempDir(), "absent"))
		m := newInstallTestManager(t)
		target, err := runtimeTarget()
		if err != nil {
			t.Fatal(err)
		}
		tree := t.TempDir()
		binaryHash := buildRuntimeTree(t, tree, target)
		archive := buildTarGz(t, tree)
		archiveHash := sha256Hex(t, archive)
		meta := RuntimeMetadata{
			Schema: 2, RuntimeVersion: "2.0.0-dev.9", Tag: "v2.0.0-dev.9", Target: target,
			ArchiveFile: runtimeArchive(target), ArchiveHash: archiveHash,
			Binary: "rsrs-runtime-" + target + binaryExtension(), BinaryHash: binaryHash,
			Compat: Compat{MinCLI: "2.0.0-dev.1", MaxCLI: "*"},
		}
		buildJSON, err := fixtureReleaseJSON(t, tree, meta)
		if err != nil {
			t.Fatal(err)
		}
		cliBytes, err := os.ReadFile(filepath.Join(tree, "rsrs-runtime-"+target))
		if err != nil {
			t.Fatal(err)
		}
		cliMeta := frontendMetadataFixture(target)
		cliMeta["binary_sha256"] = sha256Hex(t, cliBytes)
		cliMeta["binary_size"] = len(cliBytes)
		cliJSON, err := json.Marshal(cliMeta)
		if err != nil {
			t.Fatal(err)
		}
		stubCLIProbe(t, cliProbeFixture{tmpVersion: "2.0.0-dev.1", destAbsent: true})
		prefix := "https://github.com/risense-ai/respire-runtime/releases/download/" + meta.Tag + "/"
		assets := []map[string]string{
			{"name": "runtime-build-" + target + ".json", "browser_download_url": prefix + "runtime-build-" + target + ".json"},
			{"name": meta.Binary, "browser_download_url": prefix + meta.Binary},
			{"name": meta.ArchiveFile, "browser_download_url": prefix + meta.ArchiveFile},
		}
		list, err := json.Marshal([]map[string]any{{"tag_name": meta.Tag, "draft": false, "prerelease": true, "assets": assets}})
		if err != nil {
			t.Fatal(err)
		}
		cliPrefix := "https://github.com/risense-ai/respire-cli/releases/download/v2.0.0-dev.1/"
		cliAssets := []map[string]string{
			{"name": "cli-build-" + target + ".json", "browser_download_url": cliPrefix + "cli-build-" + target + ".json"},
			{"name": cliMeta["binary_file"].(string), "browser_download_url": cliPrefix + cliMeta["binary_file"].(string)},
		}
		cliList, err := json.Marshal([]map[string]any{{"tag_name": "v2.0.0-dev.1", "prerelease": true, "assets": cliAssets}})
		if err != nil {
			t.Fatal(err)
		}
		retainedMeta := meta
		retainedMeta.RuntimeVersion, retainedMeta.Tag = "2.0.0-dev.10", "v2.0.0-dev.10"
		retainedMeta.Compat.MinCLI = "2.0.0-dev.2"
		retainedJSON, err := fixtureReleaseJSON(t, tree, retainedMeta)
		if err != nil {
			t.Fatal(err)
		}
		retainedMetadataName := "runtime-build-" + target + ".json"
		retainedMetadataPath := "/risense-ai/respire-runtime/releases/download/" + retainedMeta.Tag + "/" + retainedMetadataName
		retainedReleaseJSON, err := json.Marshal(ReleaseInfo{Tag: retainedMeta.Tag, Prerelease: true,
			Assets: []ReleaseAsset{{Name: retainedMetadataName, URL: "https://github.com" + retainedMetadataPath}}})
		if err != nil {
			t.Fatal(err)
		}
		var denyCLI atomic.Bool
		var runtimeQueries atomic.Int32
		denyCLI.Store(true)
		srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
			name := r.URL.Path
			switch {
			case name == "/repos/risense-ai/respire-cli/releases" && r.URL.Query().Get("per_page") == "20":
				if denyCLI.Load() {
					t.Error("Runtime installation queried a CLI release")
					http.Error(w, "CLI release discovery unavailable", http.StatusServiceUnavailable)
					return
				}
				_, _ = w.Write(cliList)
			case name == "/repos/risense-ai/respire-runtime/releases" && r.URL.Query().Get("per_page") == "20":
				runtimeQueries.Add(1)
				_, _ = w.Write(list)
			case name == "/repos/risense-ai/respire-runtime/releases/tags/"+retainedMeta.Tag:
				_, _ = w.Write(retainedReleaseJSON)
			case name == retainedMetadataPath:
				_, _ = w.Write(retainedJSON)
			case strings.HasSuffix(name, "cli-build-"+target+".json"):
				_, _ = w.Write(cliJSON)
			case strings.HasSuffix(name, cliMeta["binary_file"].(string)):
				_, _ = w.Write(cliBytes)
			case strings.HasSuffix(name, "runtime-build-"+target+".json"):
				_, _ = w.Write(buildJSON)
			case strings.HasSuffix(name, meta.Binary):
				_, _ = w.Write(cliBytes)
			case strings.HasSuffix(name, meta.ArchiveFile):
				_, _ = w.Write(archive)
			default:
				http.NotFound(w, r)
			}
		}))
		defer srv.Close()
		m.HTTPClient = &http.Client{Transport: fakeGH{server: srv.URL}}
		if err := os.MkdirAll(filepath.Dir(m.runtimeManifest()), 0700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(m.runtimeManifest(), []byte("{broken receipt"), 0600); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(filepath.Join(m.DataRoot, "runtime.json"), []byte("{broken legacy receipt"), 0600); err != nil {
			t.Fatal(err)
		}
		incompatible := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
			_, _ = io.WriteString(w, `{"server":"respire","v":2,"pid":1}`)
		}))
		defer incompatible.Close()
		m.Endpoint = incompatible.URL
		if err := m.RunTask("existing lifecycle task", func() error {
			before := m.TaskSnapshot()
			if err := m.InstallRuntime(true, true); err == nil || !strings.Contains(err.Error(), i18n.Message47e470f70412a5d3) {
				t.Fatalf("expected activation to be blocked by the lifecycle task, got %v", err)
			}
			if err := m.verifyInstalled(InstalledRuntime{Version: meta.RuntimeVersion, SHA256: binaryHash}); err != nil {
				t.Fatalf("busy lifecycle task prevented Runtime staging: %v", err)
			}
			if m.TaskSnapshot() != before {
				t.Fatalf("Runtime staging overwrote the active task: %+v", m.TaskSnapshot())
			}
			return nil
		}); err != nil {
			t.Fatal(err)
		}
		if err = m.InstallRuntime(true, true); !errors.Is(err, errInvalidRuntimeSelection) {
			t.Fatalf("a bad selection requires explicit activation: %v", err)
		}
		if selection, err := os.ReadFile(m.runtimeManifest()); err != nil || string(selection) != "{broken receipt" {
			t.Fatalf("blocked activation changed the selection: %q, %v", selection, err)
		}
		if err = m.InstallRuntime(false, false); err != nil {
			t.Fatalf("explicit installation could not select the staged version: %v", err)
		}
		if err = m.InstallRuntime(true, true); err == nil || !strings.Contains(err.Error(), i18n.Message3a8c6a00fa74e48b) {
			t.Fatalf("incompatible Runtime must block activation: %v", err)
		}
		m.Endpoint = newFakeRuntimeServer(t, "/external/rsrs-runtime", "foreign-supervisor").URL
		if err = m.InstallRuntime(true, true); err == nil || !strings.Contains(err.Error(), i18n.Messagef64e97a3c1e5b863) {
			t.Fatalf("foreign Runtime must block activation: %v", err)
		}
		m.Endpoint = "http://127.0.0.1:1"
		installed, err := m.Installed()
		if err != nil || installed.Version != meta.RuntimeVersion || installed.SHA256 != binaryHash {
			t.Fatalf("online install pointer wrong: %+v err=%v", installed, err)
		}
		cli := filepath.Join(m.DataRoot, "bin", cliBinary())
		if _, err := os.Stat(cli); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("Runtime preparation touched the CLI: %v", err)
		}

		endpoint := filepath.Join(m.DataRoot, "runtime", "endpoint.json")
		if err := os.MkdirAll(filepath.Dir(endpoint), 0700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(endpoint, []byte("obsolete registration"), 0600); err != nil {
			t.Fatal(err)
		}
		old := InstalledRuntime{Version: "2.0.0-dev.8", SHA256: binaryHash}
		if err := writeJSON(m.runtimeManifest(), old); err != nil {
			t.Fatal(err)
		}
		queries := runtimeQueries.Load()
		if err := m.PrepareRuntimeForStartup(context.Background()); !errors.Is(err, os.ErrNotExist) || errors.Is(err, ErrRuntimeNotInstalled) {
			t.Fatalf("cold startup must preserve the bad local selection error: %v", err)
		}
		if runtimeQueries.Load() != queries {
			t.Fatal("cold startup queried releases after local verification failed")
		}
		if err := m.InstallRuntime(true, true); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("automatic activation must leave an unverifiable selection for explicit repair: %v", err)
		}
		if current, err := m.Installed(); err != nil || current != old {
			t.Fatalf("automatic activation changed the bad selection: %+v, %v", current, err)
		}
		if err := m.InstallRuntime(false, false); err != nil {
			t.Fatal(err)
		}
		m.hostAuthority = &hostAuthorityFixture{}
		library, err := m.lockRuntimeDirectory(context.Background(), m.DataRoot)
		if err != nil {
			t.Fatal(err)
		}
		queries = runtimeQueries.Load()
		startErr := m.PrepareRuntimeForStartup(context.Background())
		if err := library.Close(); err != nil {
			t.Fatal(err)
		}
		if runtimeQueries.Load() != queries {
			t.Fatal("cold startup queried releases with a verified local selection")
		}
		if startErr == nil || !strings.Contains(startErr.Error(), i18n.Messagec3e432696b223da8) {
			t.Fatalf("cold startup must report the occupied library: %v", startErr)
		}
		if _, err := os.Stat(endpoint); err != nil {
			t.Fatalf("blocked startup must retain the old endpoint record: %v", err)
		}
		if current, err := m.Installed(); err != nil || current != installed {
			t.Fatalf("blocked startup must retain the verified selection: %+v, %v", current, err)
		}

		for _, running := range []int{0, 1} {
			serving := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
				if r.Method != http.MethodGet || r.URL.Path != "/api/health" {
					t.Errorf("background check tried to change Runtime: %s %s", r.Method, r.URL.Path)
					http.NotFound(w, r)
					return
				}
				err := json.NewEncoder(w).Encode(map[string]any{
					"server": "respire", "v": 1, "pid": os.Getpid(), "bin": installed.Version,
					"runtime_version": installed.Version, "image_sha256": installed.SHA256,
					"installation_root": m.DataRoot, "managed_id": "cli-supervisor",
					"exe": filepath.Join(m.runtimeDir(installed.Version), runtimeBinary()),
					"rpc": map[string]int{"executing_until_reply": running, "queued_or_cancelled_pending_removal": 0},
				})
				if err != nil {
					t.Error(err)
				}
			}))
			m.Endpoint = serving.URL
			before := m.TaskSnapshot()
			err := m.InstallRuntime(true, true)
			serving.Close()
			if err != nil || m.TaskSnapshot() != before {
				t.Fatalf("same verified Runtime must keep task state with %d active requests: %v, %+v", running, err, m.TaskSnapshot())
			}
		}
		before := m.TaskSnapshot()
		if err := m.RunTask(i18n.Messageb5e0512f87d71201, func() error { return errRuntimeUpdateDeferred }); err != nil || m.TaskSnapshot() != before {
			t.Fatalf("deferred activation must retain previous task state: %v, %+v", err, m.TaskSnapshot())
		}
		denyCLI.Store(false)
		if err := m.installOnlineCLI(context.Background()); err != nil {
			t.Fatalf("separate CLI installation failed: %v", err)
		}
		t.Run("startup missing CLI downloads only host", func(t *testing.T) {
			fresh := newInstallTestManager(t)
			fresh.HTTPClient = m.HTTPClient
			t.Setenv("RESPIRE_TEST_RECOVERY_CLI", "1")
			t.Setenv("RESPIRE_TEST_RECOVERY_CALLS", filepath.Join(t.TempDir(), "calls"))
			if err := fresh.InstallCLI(context.Background()); err != nil {
				t.Fatal("first CLI preparation failed", err)
			}
			if _, err := os.Stat(fresh.runtimeManifest()); !errors.Is(err, os.ErrNotExist) {
				t.Fatal("CLI preparation selected a Runtime", err)
			}
			if _, err := os.Stat(fresh.runtimeDir(meta.RuntimeVersion)); !errors.Is(err, os.ErrNotExist) {
				t.Fatal("CLI preparation installed a Runtime", err)
			}
		})
		for _, runtimeVersion := range []string{meta.RuntimeVersion, retainedMeta.RuntimeVersion} {
			t.Run("CLI preparation is independent of Runtime "+runtimeVersion, func(t *testing.T) {
				t.Setenv("RESPIRE_TEST_RECOVERY_CLI", "1")
				t.Setenv("RESPIRE_TEST_CORE_UNSUPPORTED", "1")
				calls := filepath.Join(t.TempDir(), "calls")
				t.Setenv("RESPIRE_TEST_RECOVERY_CALLS", calls)
				stubCLIProbe(t, cliProbeFixture{tmpVersion: "2.0.0-dev.1", destVer: "1.0.18"})
				health := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
					t.Errorf("CLI-only preparation accessed Runtime: %s %s", r.Method, r.URL.Path)
					_ = json.NewEncoder(w).Encode(HealthInfo{Server: "respire", Protocol: 1, PID: 4242,
						Version: runtimeVersion, Exe: filepath.Join(tree, "rsrs-runtime-"+target), ManagedID: "fixture-supervisor"})
				}))
				defer health.Close()
				before, err := os.ReadFile(m.runtimeManifest())
				if err != nil {
					t.Fatal(err)
				}
				previousEndpoint := m.Endpoint
				m.Endpoint = health.URL
				defer func() { m.Endpoint = previousEndpoint }()
				if err := m.InstallCLI(context.Background()); err != nil {
					t.Fatal("standalone CLI could not be prepared", err)
				}
				if _, err := os.Stat(calls); !errors.Is(err, os.ErrNotExist) {
					t.Fatal("CLI preparation invoked a business command", err)
				}
				after, err := os.ReadFile(m.runtimeManifest())
				if err != nil || string(before) != string(after) {
					t.Fatal("CLI-only preparation changed Runtime selection", err)
				}
				if err := m.verifyInstalled(installed); err != nil {
					t.Fatal("CLI-only preparation changed Runtime bytes", err)
				}
			})
		}
		t.Run("same version verifies against incoming release before repairing receipt", func(t *testing.T) {
			path := m.runtimeReceiptPath(meta.RuntimeVersion)
			receipt, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(func() {
				if err := os.WriteFile(path, receipt, 0600); err != nil {
					t.Errorf("restore receipt fixture: %v", err)
				}
			})
			if err = os.Remove(path); err != nil {
				t.Fatal(err)
			}
			err = m.InstallRuntime(false, false)
			if err != nil {
				t.Fatalf("verified incoming release can repair its missing receipt: %v", err)
			}
			if err := m.verifyInstalled(installed); err != nil {
				t.Fatalf("repaired receipt did not verify: %v", err)
			}
			if current, err := m.Installed(); err != nil || current != installed {
				t.Fatalf("verification failure changed the selection: %+v %v", current, err)
			}
		})
		t.Run("same version with damaged binary fails without replacing it", func(t *testing.T) {
			path := filepath.Join(m.runtimeDir(meta.RuntimeVersion), runtimeBinary())
			original, err := os.ReadFile(path)
			if err != nil {
				t.Fatal(err)
			}
			t.Cleanup(func() {
				if err := os.WriteFile(path, original, 0700); err != nil {
					t.Errorf("restore Runtime fixture: %v", err)
				}
			})
			if err = os.WriteFile(path, []byte("damaged"), 0700); err != nil {
				t.Fatal(err)
			}
			err = m.InstallRuntime(false, false)
			if err == nil {
				t.Fatalf("damaged Runtime must expose its verification error: %v", err)
			}
			data, err := os.ReadFile(path)
			if err != nil || string(data) != "damaged" {
				t.Fatalf("verification failure must preserve the damaged file: %q %v", data, err)
			}
		})
		t.Run("without restart reuse leaves an older running instance alone", func(t *testing.T) {
			stubCLIProbe(t, cliProbeFixture{tmpVersion: "2.0.0-dev.1", destVer: "2.0.0-dev.1"})
			olderExe := filepath.Join(m.runtimeDir("2.0.0-dev.8"), runtimeBinary())
			m.Endpoint = newFakeRuntimeServer(t, olderExe, "").URL
			if err := m.InstallRuntime(false, false); err != nil {
				t.Fatalf("reuse must not try to stop the running instance: %v", err)
			}
			if current, err := m.Installed(); err != nil || current != installed {
				t.Fatalf("reuse changed the selected Runtime: %+v %v", current, err)
			}
		})
		t.Run("without restart reuse of a stopped instance returns for startup", func(t *testing.T) {
			stubCLIProbe(t, cliProbeFixture{tmpVersion: "2.0.0-dev.1", destVer: "2.0.0-dev.1"})
			m.Endpoint = "http://127.0.0.1:1"
			if err := m.InstallRuntime(false, false); err != nil {
				t.Fatalf("reuse of a stopped Runtime must leave startup to the caller: %v", err)
			}
		})
		entries, err := filepath.Glob(filepath.Join(m.Root, ".cli-download-*"))
		if err != nil || len(entries) != 0 {
			t.Fatalf("CLI download staging leftovers: %v %v", entries, err)
		}
		if err = os.Remove(cli); err != nil {
			t.Fatal(err)
		}
		m.Endpoint = newFakeRuntimeServer(t, filepath.Join(m.runtimeDir(meta.RuntimeVersion), runtimeBinary()), "").URL
		if err = m.installOnlineCLI(context.Background()); err != nil {
			t.Fatalf("online CLI repair with existing Runtime failed: %v", err)
		}
		if hash, err := fileHash(cli); err != nil || hash != cliMeta["binary_sha256"] {
			t.Fatalf("online CLI was not restored: %s %v", hash, err)
		}
		if err = copyTree(m.runtimeDir(meta.RuntimeVersion), m.runtimeDir(retainedMeta.RuntimeVersion)); err != nil {
			t.Fatal(err)
		}
		retained := InstalledRuntime{Version: retainedMeta.RuntimeVersion, SHA256: binaryHash}
		retainedProof, err := NormalizeRuntimeMetadata(retainedJSON)
		if err != nil {
			t.Fatal(err)
		}
		retainedReceipt, err := runtimeReceipt(retainedProof)
		if err != nil {
			t.Fatal(err)
		}
		if err = m.saveRuntimeReceipt(retainedReceipt); err != nil {
			t.Fatal(err)
		}
		if err = writeJSON(m.runtimeManifest(), retainedReceipt); err != nil {
			t.Fatal(err)
		}
		if err = os.Remove(cli); err != nil {
			t.Fatal(err)
		}
		if err := m.InstallRuntime(false, false); err != nil {
			t.Fatalf("verified newer Runtime should remain selected: %v", err)
		}
		if current, err := m.Installed(); err != nil || current != retained {
			t.Fatalf("staging downgraded the selected Runtime: %+v %v", current, err)
		}
		if _, err := os.Stat(cli); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("retaining Runtime attempted to repair the CLI: %v", err)
		}
		if err := os.Remove(m.runtimeReceiptPath(retained.Version)); err != nil {
			t.Fatal(err)
		}
		if err := os.Remove(m.runtimeReceiptPath(installed.Version)); err != nil {
			t.Fatal(err)
		}
		if err := m.InstallRuntime(false, false); err == nil || !strings.Contains(err.Error(), i18n.Message634944eda505ae3f) {
			t.Fatalf("invalid retained release must report the activation error after staging: %v", err)
		}
		if err := m.verifyInstalled(installed); err != nil {
			t.Fatalf("incoming release must be staged before verifying the retained release: %v", err)
		}
		if current, err := m.Installed(); err != nil || current != retained {
			t.Fatalf("staging changed the selected Runtime: %+v %v", current, err)
		}
		for _, failure := range []string{"hash", "size"} {
			cliMeta["binary_sha256"] = sha256Hex(t, cliBytes)
			cliMeta["binary_size"] = len(cliBytes)
			meta.Compat.MinCLI = "2.0.0-dev.1"
			switch failure {
			case "hash":
				cliMeta["binary_sha256"] = strings.Repeat("0", 64)
			case "size":
				cliMeta["binary_size"] = len(cliBytes) + 1
			}
			cliJSON, err = json.Marshal(cliMeta)
			if err != nil {
				t.Fatal(err)
			}
			buildJSON, err = fixtureReleaseJSON(t, tree, meta)
			if err != nil {
				t.Fatal(err)
			}
			rejected := newInstallTestManager(t)
			rejected.HTTPClient = m.HTTPClient
			if err = rejected.installOnlineCLI(context.Background()); err == nil {
				t.Fatalf("invalid CLI %s must reject the CLI", failure)
			}
			if _, readErr := rejected.Installed(); !errors.Is(readErr, os.ErrNotExist) {
				t.Fatalf("invalid CLI %s published a Runtime selection: %v", failure, readErr)
			}
			if _, statErr := os.Stat(filepath.Join(rejected.DataRoot, "bin", cliBinary())); !errors.Is(statErr, os.ErrNotExist) {
				t.Fatalf("invalid CLI %s was published: %v", failure, statErr)
			}
			entries, globErr := filepath.Glob(filepath.Join(rejected.Root, ".cli-download-*"))
			if globErr != nil || len(entries) != 0 {
				t.Fatalf("failed CLI %s left download staging: %v %v", failure, entries, globErr)
			}
		}
	})
}

func sha256Hex(t *testing.T, b []byte) string {
	t.Helper()
	sum := sha256.Sum256(b)
	return hex.EncodeToString(sum[:])
}

// newFakeRuntimeServer answers /api/health with a minimal valid health
// identity for the ownership checks.
func newFakeRuntimeServer(t *testing.T, exe, managedID string) *httptest.Server {
	t.Helper()
	srv := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		if r.URL.Path == "/api/health" && r.Header.Get("X-Respire-Health") != "runtime-v1" {
			t.Error("health must request the actual Runtime identity")
		}
		w.Header().Set("Content-Type", "application/json")
		_, _ = fmt.Fprintf(w, `{"server":"respire","v":1,"pid":4242,"exe":%q,"managed_id":%q}`, exe, managedID)
	}))
	t.Cleanup(srv.Close)
	return srv
}
