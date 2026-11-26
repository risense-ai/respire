package host

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"
)

// This opt-in fixture uses the existing synthetic release inventory, never a
// signed release claim or a replacement for application verification.
func realRuntimeManager(t *testing.T) (*Manager, InstalledRuntime) {
	t.Helper()
	return realRuntimeManagerAt(t, nil)
}

func realRuntimeManagerAt(t *testing.T, configure func(*testing.T, string) string) (*Manager, InstalledRuntime) {
	t.Helper()
	image := os.Getenv("RESPIRE_TEST_REAL_RUNTIME")
	if image == "" {
		t.Skip("set RESPIRE_TEST_REAL_RUNTIME and RESPIRE_TEST_REAL_RUNTIME_VERSION for native acceptance")
	}
	version := os.Getenv("RESPIRE_TEST_REAL_RUNTIME_VERSION")
	if !filepath.IsAbs(image) || !validVersion(version) {
		t.Fatal("real Runtime requires an absolute image path and its exact build version")
	}
	root, err := canonicalSessionDirectory(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	for _, key := range []string{"HOME", "USERPROFILE", "APPDATA", "LOCALAPPDATA", "RSRS_DATA_DIR"} {
		t.Setenv(key, root)
	}
	// Do not inherit credentials or host configuration into either Rust child.
	for _, entry := range os.Environ() {
		key, _, _ := strings.Cut(entry, "=")
		upper := strings.ToUpper(key)
		if strings.HasPrefix(upper, "RSRS_") || strings.HasPrefix(upper, "ONEMEMORY_") || strings.HasPrefix(upper, "RESPIRE_") ||
			strings.HasSuffix(upper, "_API_KEY") || strings.HasSuffix(upper, "_TOKEN") || strings.HasSuffix(upper, "_SECRET") {
			t.Setenv(key, "")
		}
	}
	if configure != nil {
		root = configure(t, root)
	}
	t.Setenv("RSRS_DATA_DIR", root)
	listener, err := net.Listen("tcp4", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	port := listener.Addr().(*net.TCPAddr).Port
	if err := listener.Close(); err != nil {
		t.Fatal(err)
	}
	t.Setenv("RSRS_RPC_PORT", strconv.Itoa(port))
	m, err := New()
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(m.profile.close)
	if m.hostAuthority != nil || m.startupControl == nil {
		t.Fatal("production New wiring changed")
	}
	denyOutbound := cliMetadataTransport(func(*http.Request) (*http.Response, error) {
		return nil, errors.New("outbound HTTP forbidden in disposable native acceptance")
	})
	m.HTTPClient.Transport, m.modelHTTPClient.Transport = denyOutbound, denyOutbound
	t.Cleanup(m.localClient.CloseIdleConnections)
	target, err := runtimeTarget()
	if err != nil {
		t.Fatal(err)
	}
	tree := t.TempDir()
	buildRuntimeTree(t, tree, target)
	raw, err := os.ReadFile(image)
	if err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"rsrs-runtime-" + target, runtimeBinary()} {
		if err := os.WriteFile(filepath.Join(tree, name), raw, 0700); err != nil {
			t.Fatal(err)
		}
	}
	hash, err := fileHash(image)
	if err != nil {
		t.Fatal(err)
	}
	meta := RuntimeMetadata{Schema: 2, RuntimeVersion: version, Tag: "v" + version, Target: target,
		ArchiveFile: runtimeArchive(target), ArchiveHash: strings.Repeat("0", 64),
		Binary: "rsrs-runtime-" + target + binaryExtension(), BinaryHash: hash,
		Compat: Compat{MinCLI: "0.4.0", MaxCLI: "*"}}
	proof, err := fixtureReleaseJSON(t, tree, meta)
	if err != nil {
		t.Fatal(err)
	}
	meta, err = NormalizeRuntimeMetadata(proof)
	if err != nil {
		t.Fatal(err)
	}
	receipt, err := runtimeReceipt(meta)
	if err != nil {
		t.Fatal(err)
	}
	if err := m.publishRuntime(meta, func(stage string) error { return copyReleaseTree(tree, stage, receipt) }, false); err != nil {
		t.Fatal(err)
	}
	info := InstalledRuntime{Version: version, SHA256: hash}
	if err := m.selectRuntime(info); err != nil {
		t.Fatal(err)
	}
	if err := m.verifyRuntimeSelection(info); err != nil {
		t.Fatal(err)
	}
	t.Logf("synthetic disposable installation=%s version=%s image_sha256=%s", root, version, hash)
	return m, info
}

func realRuntimeContext(t *testing.T) context.Context {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 90*time.Second)
	t.Cleanup(cancel)
	return ctx
}

func TestRealRuntimeStartupControl(t *testing.T) {
	m, info := realRuntimeManager(t)
	ctx := realRuntimeContext(t)
	guard, err := m.acquireStartup(ctx, info)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := guard.Close(); err != nil {
			t.Error(err)
		}
	})
	t.Logf("real held scope: %+v", guard.Held())
	waitCtx, cancel := context.WithTimeout(ctx, 500*time.Millisecond)
	other, err := m.acquireStartup(waitCtx, info)
	cancel()
	if other != nil {
		other.Close()
		t.Fatal("second real Rust helper acquired the held exclusion")
	}
	if !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("contended acquisition did not preserve deadline: %v", err)
	}
	released, err := guard.Finish(nil)
	if err != nil || released.Endpoint != nil {
		t.Fatalf("real abort receipt: %+v, %v", released, err)
	}
	requireStartupReaped(t, guard)
	next, err := m.acquireStartup(ctx, info)
	if err != nil {
		t.Fatalf("exclusion not released after abort/cancel: %v", err)
	}
	if _, err := next.Finish(nil); err != nil {
		next.Close()
		t.Fatal(err)
	}
	requireStartupReaped(t, next)
	invalid, err := m.acquireStartup(ctx, info)
	if err != nil {
		t.Fatal(err)
	}
	// A non-null claim cannot complete without a real durable publication.
	endpoint := startupEndpoint{URL: m.Endpoint}
	if receipt, err := invalid.Finish(&endpoint); err == nil || receipt != (startupReleased{}) {
		t.Fatalf("missing endpoint publication accepted: %+v, %v", receipt, err)
	}
	requireStartupReaped(t, invalid)
	recovered, err := m.acquireStartup(ctx, info)
	if err != nil {
		t.Fatalf("failed endpoint completion retained exclusion: %v", err)
	}
	if _, err := recovered.Finish(nil); err != nil {
		recovered.Close()
		t.Fatal(err)
	}
	requireStartupReaped(t, recovered)
	t.Log("real Rust contention, cancellation, abort receipt, reap and reacquisition verified")
}

func TestRealRuntimeSelectedProfile(t *testing.T) {
	m, info := realRuntimeManager(t)
	profile := filepath.Join(m.DataRoot, "accounts", "disposable")
	if err := os.MkdirAll(profile, 0700); err != nil {
		t.Fatal(err)
	}
	if err := writeJSON(filepath.Join(m.DataRoot, "client.json"), map[string]string{"data_dir": profile}); err != nil {
		t.Fatal(err)
	}
	guard, err := m.acquireStartup(realRuntimeContext(t), info)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() { guard.Close() })
	if held := guard.Held(); !samePath(held.Profile, profile) || held.AccountID != controlDigest([]byte(held.Profile)) {
		t.Fatalf("selected accountless profile scope mismatch: %+v", held)
	}
	if _, err := guard.Finish(nil); err != nil {
		t.Fatal(err)
	}
	requireStartupReaped(t, guard)
}

func cleanupRealRuntime(t *testing.T, m *Manager, info InstalledRuntime) {
	t.Helper()
	health := m.directRuntime
	if health == (HealthInfo{}) {
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), 35*time.Second)
	defer cancel()
	process, err := os.FindProcess(int(health.PID))
	if err != nil {
		t.Error(err)
		return
	}
	defer process.Release()
	_, profile, account, err := m.startupScope()
	if err == nil {
		err = m.verifyScopedRuntimeOwner(ctx, health, info, profile, account)
	}
	if err != nil {
		t.Errorf("cleanup cannot verify test-owned child: %v", err)
		return
	}
	if err := m.stopDetached(ctx, health); err != nil {
		t.Errorf("owned Runtime cooperative stop/receipt failed: %v", err)
		// Report failure and terminate only the verified test-owned process handle.
		if killErr := process.Kill(); killErr != nil && !errors.Is(killErr, os.ErrProcessDone) {
			t.Errorf("owned child emergency cleanup failed: %v", killErr)
		}
	}
	if err := requireClosedRuntimePort(ctx, strings.TrimPrefix(m.Endpoint, "http://")); err != nil {
		t.Error(err)
	}
}

func TestRealRuntimeColdLaunch(t *testing.T) {
	m, info := realRuntimeManager(t)
	ctx := realRuntimeContext(t)
	t.Cleanup(func() { cleanupRealRuntime(t, m, info) })
	if err := requireClosedRuntimePort(ctx, strings.TrimPrefix(m.Endpoint, "http://")); err != nil {
		t.Fatal(err)
	}
	if err := m.PrepareRuntimeForStartup(ctx); err != nil {
		logs, globErr := filepath.Glob(filepath.Join(m.Root, "runtime-start-*.log"))
		if globErr != nil {
			t.Error(globErr)
		}
		for _, path := range logs {
			raw, readErr := os.ReadFile(path)
			if readErr != nil {
				t.Error(readErr)
			}
			t.Logf("serving diagnostic: %s", raw)
		}
		if m.directRuntime != (HealthInfo{}) {
			t.Error("failed launch retained direct ownership")
		}
		cleanupCtx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		if closedErr := requireClosedRuntimePort(cleanupCtx, strings.TrimPrefix(m.Endpoint, "http://")); closedErr != nil {
			t.Error(closedErr)
		}
		guard, cleanupErr := m.acquireStartup(cleanupCtx, info)
		if cleanupErr != nil {
			t.Errorf("failed launch retained startup exclusion: %v", cleanupErr)
		} else {
			if _, finishErr := guard.Finish(nil); finishErr != nil {
				t.Error(finishErr)
			}
			guard.Close()
			requireStartupReaped(t, guard)
		}
		t.Fatalf("real cold launch NOT accepted: %v", err)
	}
	health := m.directRuntime
	root, profile, account, err := m.startupScope()
	if err != nil || !samePath(root, m.DataRoot) {
		t.Fatalf("scope: %s, %v", root, err)
	}
	if err := m.verifyScopedRuntimeOwner(ctx, health, info, profile, account); err != nil {
		t.Fatal(err)
	}
	t.Logf("real startup complete: %+v", health)
	publication := filepath.Join(m.DataRoot, "runtime", "endpoint.json")
	before, err := os.ReadFile(publication)
	if err != nil {
		t.Fatal(err)
	}
	var endpoint startupEndpoint
	if err := json.Unmarshal(before, &endpoint); err != nil {
		t.Fatal(err)
	}
	want := startupEndpoint{health.PID, health.ProcessStartID, health.ImagePath, health.ImageSHA256,
		health.InstanceID, health.AccountID, health.Generation, m.Endpoint}
	if endpoint != want {
		t.Fatalf("durable endpoint differs from live identity: %+v", endpoint)
	}
	guard, err := m.acquireStartup(ctx, info)
	if err != nil {
		t.Fatal(err)
	}
	if receipt, err := guard.Finish(&want); err != nil || receipt.Endpoint == nil || *receipt.Endpoint != want {
		guard.Close()
		t.Fatalf("exact live endpoint completion: %+v, %v", receipt, err)
	}
	requireStartupReaped(t, guard)
	if health.StoreReady {
		t.Fatal("accountless startup unexpectedly unlocked a store")
	}
	// Invalid payloads exercise authentication without unlocking a store.
	token, err := m.runtimeUnlockToken()
	if err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		name, token string
		status      int
	}{{"missing", "", 401}, {"wrong", "disposable-invalid", 401}, {"authorized malformed body", token, http.StatusConflict}} {
		req, err := http.NewRequestWithContext(ctx, http.MethodPost, m.Endpoint+"/api/runtime/unlock", strings.NewReader("{}"))
		if err != nil {
			t.Fatal(err)
		}
		if tc.token != "" {
			req.Header.Set("Authorization", "Bearer "+tc.token)
		}
		response, err := m.localClient.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		response.Body.Close()
		if response.StatusCode != tc.status {
			t.Fatalf("%s authentication HTTP %d, want %d", tc.name, response.StatusCode, tc.status)
		}
	}
	if err := m.PrepareRuntimeForStartup(ctx); err != nil {
		t.Fatal(err)
	}
	reused, _, err := m.Health(ctx)
	if err != nil || reused != health {
		t.Fatalf("same-process discovery changed owner: %+v, %v", reused, err)
	}
	after, err := os.ReadFile(publication)
	if err != nil || string(after) != string(before) {
		t.Fatalf("discovery rewrote endpoint publication: %v", err)
	}
}

func TestRealRuntimeStandaloneStop(t *testing.T) {
	manager, info := realRuntimeManager(t)
	ctx := realRuntimeContext(t)
	health, err := manager.startDetached(ctx, info)
	if err != nil {
		t.Fatal(err)
	}
	stopped := false
	t.Cleanup(func() {
		if !stopped {
			cleanupRealRuntime(t, manager, info)
		}
	})
	if manager.hostAuthority != nil || manager.startupControl == nil {
		t.Fatal("test did not use the production standalone Runtime path")
	}
	wrong := health
	wrong.ProcessStartID += "-mismatch"
	if err := manager.stopSelectedDetached(ctx, wrong); err == nil {
		t.Fatal("wrong process identity was allowed to request Stop")
	}
	current, _, err := manager.Health(ctx)
	if err != nil || current != health {
		t.Fatalf("invalid Stop changed servicing process: %v", err)
	}
	if err := manager.Stop(true); err != nil {
		t.Fatalf("owned standalone Stop failed: %v", err)
	}
	stopped = true
	if err := requireClosedRuntimePort(ctx, strings.TrimPrefix(manager.Endpoint, "http://")); err != nil {
		t.Fatal(err)
	}
}
