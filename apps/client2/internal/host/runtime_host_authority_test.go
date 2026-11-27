package host

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net"
	"os"
	"path/filepath"
	"strconv"
	"testing"
	"time"
)

// Models the caller contract for tests. It does not test exclusion with Rust.
type hostAuthorityFixture struct {
	held map[string]*hostOwnershipFixture
}

type hostOwnershipFixture struct {
	authority *hostAuthorityFixture
	directory string
	kind      runtimeOwnershipKind
	wait      time.Duration
}

var errFixtureOwnershipBusy = errors.New("fixture ownership is busy")

func (f *hostAuthorityFixture) acquire(_ context.Context, kind runtimeOwnershipKind, root, directory string, wait time.Duration) (io.Closer, error) {
	if f.held[directory] != nil {
		return nil, errFixtureOwnershipBusy
	}
	if kind == runtimeTakeoverOwnership {
		if err := os.MkdirAll(filepath.Join(root, "runtime"), 0700); err != nil {
			return nil, err
		}
	}
	if f.held == nil {
		f.held = make(map[string]*hostOwnershipFixture)
	}
	lock := &hostOwnershipFixture{authority: f, directory: directory, kind: kind, wait: wait}
	f.held[directory] = lock
	return lock, nil
}

func (lock *hostOwnershipFixture) Close() error {
	if lock.authority.held[lock.directory] != lock {
		return errors.New("fixture ownership is already closed")
	}
	delete(lock.authority.held, lock.directory)
	return nil
}

func TestRuntimeHostTakeoverOwnership(t *testing.T) {
	root, err := canonicalSessionDirectory(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	m := Manager{DataRoot: root}
	ctx, cancel := context.WithTimeout(context.Background(), time.Minute)
	defer cancel()
	if _, err := m.lockRuntimeTakeover(ctx); !errors.Is(err, errRuntimeHostAuthorityUnavailable) {
		t.Fatalf("missing authority: %v", err)
	}
	if _, err := os.Stat(filepath.Join(root, "runtime")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("missing authority changed installation: %v", err)
	}
	fixture := &hostAuthorityFixture{}
	m.hostAuthority = fixture
	lock, err := m.lockRuntimeTakeover(ctx)
	if err != nil {
		t.Fatal(err)
	}
	dir := filepath.Join(root, "runtime", "takeover")
	held := fixture.held[dir]
	if lock != held || held.kind != runtimeTakeoverOwnership || held.wait <= 0 || held.wait > time.Minute {
		t.Fatalf("incorrect takeover binding: %+v", held)
	}
	cancel()
	if _, err := m.lockRuntimeTakeover(context.Background()); !errors.Is(err, errFixtureOwnershipBusy) {
		t.Fatalf("competing takeover was admitted or error lost: %v", err)
	}
	if err := lock.Close(); err != nil {
		t.Fatal(err)
	}
	next, err := m.lockRuntimeTakeover(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if err := next.Close(); err != nil {
		t.Fatal(err)
	}
}

func TestOfflineAccountAdmissionRequiresHostOwnership(t *testing.T) {
	root, err := canonicalSessionDirectory(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	m := Manager{DataRoot: root}
	intent := runtimeLoginIntent{
		OperationID: "25bf4cf1-9865-47fa-bf59-5147307883f9", User: "fixture", Server: "https://example.invalid",
	}
	_, lock, err := m.beginOfflineRuntimeAccountMaintenance(context.Background(), root, "test-issuer", intent)
	if lock != nil || !errors.Is(err, errRuntimeHostAuthorityUnavailable) {
		t.Fatalf("offline maintenance continued without authority: %v", err)
	}
	if _, err := os.Stat(filepath.Join(root, "runtime")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("failed admission published Runtime state: %v", err)
	}
}

func TestWorkspaceModeResolution(t *testing.T) {
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("RSRS_DATA_DIR", root)
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("RSRS_RPC_PORT", strconv.Itoa(listener.Addr().(*net.TCPAddr).Port))
	if err := listener.Close(); err != nil {
		t.Fatal(err)
	}
	manager, err := New()
	if err != nil {
		t.Fatal(err)
	}
	manager.hostAuthority = &hostAuthorityFixture{}
	manager.ClientVersion = "2.0.0-dev.1"
	mode := func() string {
		value, err := manager.WorkspaceMode()
		if err != nil {
			t.Fatal(err)
		}
		return value
	}
	if mode := mode(); mode != "normal" {
		t.Fatalf("default mode = %s", mode)
	}
	if err := manager.SetWorkspaceMode(t.Context(), "readonly"); err != nil {
		t.Fatal(err)
	}
	if mode := mode(); mode != "readonly" {
		t.Fatalf("mode after set = %s", mode)
	}
	if err := manager.SetWorkspaceMode(t.Context(), "off"); err != nil {
		t.Fatal(err)
	}
	if mode := mode(); mode != "off" {
		t.Fatalf("mode after off = %s", mode)
	}
	if err := manager.SetWorkspaceMode(t.Context(), "nope"); err == nil {
		t.Fatal("invalid mode accepted")
	}
	// agent.json flags count only when no service_mode is stored
	config, err := readAgentSettingsObject(filepath.Join(root, "client.json"))
	if err != nil {
		t.Fatal(err)
	}
	delete(config, "service_mode")
	data, err := json.Marshal(config)
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "client.json"), data, 0o600); err != nil {
		t.Fatal(err)
	}
	dataDir := filepath.Join(root, "lib")
	if err := os.MkdirAll(dataDir, 0o700); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(dataDir, "agent.json"), []byte(`{"memory_off": true}`), 0o600); err != nil {
		t.Fatal(err)
	}
	config["data_dir"] = dataDir
	if data, err = json.Marshal(config); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(root, "client.json"), data, 0o600); err != nil {
		t.Fatal(err)
	}
	if mode := mode(); mode != "off" {
		t.Fatalf("agent.json fallback mode = %s", mode)
	}
}
