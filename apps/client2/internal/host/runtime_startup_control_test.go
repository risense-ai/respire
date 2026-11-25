package host

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"io"
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

// A copied Go test image checks the adapter, not Rust exclusion or listener verification.
func startupControlFixture() int {
	mode := os.Getenv("RESPIRE_TEST_STARTUP_CONTROL")
	if mode == "" {
		return 2
	}
	var request startupAcquire
	if readStartupFrame(os.Stdin, &request) != nil || request.SchemaVersion != 1 {
		return 3
	}
	if os.Getenv("RSRS_RUNTIME_BOOT_TOKEN") != "" || os.Getenv("RSRS_SUPER") != "" ||
		!samePath(os.Getenv("RSRS_DATA_DIR"), request.InstallationRoot) || os.Getenv("RSRS_RPC_PORT") != "1" {
		return 4
	}
	birth, err := hostControlBirth(context.Background())
	if err != nil {
		return 5
	}
	controller, err := runtimeControlProcessBirth(context.Background(), int(request.ControllerPID))
	if err != nil || controller != request.ControllerBirth {
		return 6
	}
	if err := writeJSON(filepath.Join(request.InstallationRoot, "fixture-helper.json"), map[string]int{"pid": os.Getpid()}); err != nil {
		return 15
	}
	if mode == "wait" {
		time.Sleep(time.Hour)
		return 7
	}
	if err := os.MkdirAll(request.Profile, 0700); err != nil {
		return 8
	}
	identity, err := deviceProfileIdentity(request.Profile)
	if err != nil {
		return 9
	}
	account := controlDigest([]byte(request.Profile))
	if identity.AccountID != nil {
		account = *identity.AccountID
	}
	held := startupHeld{1, "fb9fa5fc-e596-431c-a562-aa24d32058e7", request.InstallationRoot,
		request.Profile, account, uint32(os.Getpid()), birth}
	switch mode {
	case "bad-pid":
		held.HelperPID++
	case "bad-birth":
		held.HelperBirth = "wrong"
	case "bad-profile":
		held.Profile = filepath.Join(request.Profile, "other")
	case "bad-account":
		held.AccountID = strings.Repeat("0", 64)
	case "bad-root":
		held.InstallationRoot = filepath.Dir(request.InstallationRoot)
	case "bad-schema":
		held.SchemaVersion++
	case "bad-guard":
		held.GuardID = "invalid"
	}
	if writeStartupFrame(os.Stdout, held) != nil {
		return 10
	}
	if mode == "held-exit" {
		return 16
	}
	var finish startupFinish
	if readStartupFrame(os.Stdin, &finish) != nil || finish.GuardID != held.GuardID {
		return 11
	}
	if mode == "finish-wait" {
		time.Sleep(time.Hour)
		return 12
	}
	released := startupReleased{1, held.GuardID, finish.Endpoint}
	if mode == "bad-release" {
		released.GuardID = "different"
	}
	if mode == "bad-endpoint" && released.Endpoint != nil {
		released.Endpoint.Generation++
	}
	if writeStartupFrame(os.Stdout, released) != nil {
		return 13
	}
	if mode == "trailing" {
		os.Stdout.Write([]byte{0})
	}
	if mode == "exit-failure" {
		return 14
	}
	return 0
}

func startupServingFixture() int {
	root := os.Getenv("RSRS_DATA_DIR")
	profile, err := selectedRuntimeLoginProfile(root)
	if err != nil {
		return 1
	}
	image, err := os.Executable()
	if err != nil {
		return 2
	}
	image, err = canonicalSessionDirectory(image)
	if err != nil {
		return 3
	}
	hash, err := fileHash(image)
	if err != nil {
		return 4
	}
	birth, err := hostControlBirth(context.Background())
	if err != nil {
		return 5
	}
	identity, err := deviceProfileIdentity(profile)
	if err != nil {
		return 6
	}
	account := controlDigest([]byte(profile))
	if identity.AccountID != nil {
		account = *identity.AccountID
	}
	health := HealthInfo{Server: "respire", PID: uint32(os.Getpid()), Version: "1.0.21", RuntimeVersion: "1.0.21", Protocol: 1,
		Exe: image, ImagePath: image, ImageSHA256: hash, ProcessStartID: runtimeBirthID(birth),
		InstallationRoot: root, InstallationID: controlDigest([]byte(root)), DataDir: profile,
		AccountID: account, InstanceID: "ca779cd5-a879-4f15-a43c-65eb2dbd87ed", Generation: 1, StartupComplete: os.Getenv("RESPIRE_TEST_STARTUP_PENDING") != "1"}
	if err := writeJSON(filepath.Join(root, "fixture-health.json"), health); err != nil {
		return 7
	}
	for {
		if _, err := os.Stat(filepath.Join(root, "fixture-stop")); err == nil {
			return 0
		} else if !errors.Is(err, os.ErrNotExist) {
			return 8
		}
		time.Sleep(10 * time.Millisecond)
	}
}

func startupTestContext(t *testing.T) context.Context {
	t.Helper()
	ctx, cancel := context.WithTimeout(t.Context(), 5*time.Second)
	t.Cleanup(cancel)
	return ctx
}

func requireStartupReaped(t *testing.T, guard *runtimeStartup) {
	t.Helper()
	if !guard.done || guard.cmd.ProcessState == nil {
		t.Fatal("startup helper was not reaped")
	}
	birth, err := runtimeControlProcessBirth(startupTestContext(t), guard.cmd.Process.Pid)
	if err != nil || birth != "" {
		t.Fatalf("startup helper is still alive: %q, %v", birth, err)
	}
}

func requireFixtureHelperExited(t *testing.T, m *Manager) {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(m.DataRoot, "fixture-helper.json"))
	if err != nil {
		t.Fatal(err)
	}
	var child struct {
		PID int `json:"pid"`
	}
	if err := json.Unmarshal(raw, &child); err != nil || child.PID <= 0 {
		t.Fatalf("fixture helper identity: %v", err)
	}
	birth, err := runtimeControlProcessBirth(startupTestContext(t), child.PID)
	if err != nil || birth != "" {
		t.Fatalf("fixture helper retained: %q, %v", birth, err)
	}
}

func TestStartupAccountProjection(t *testing.T) {
	profile, err := canonicalSessionDirectory(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	for _, tc := range []struct {
		session string
		account string
		valid   bool
	}{
		{`{"user":"synthetic"}`, controlDigest([]byte("synthetic")), true},
		// Runtime, not this projection, must validate the migrated claim and original receipt.
		{`{"vault_version":4,"keyring_account":"legacy-0000000000000000"}`, controlDigest([]byte(profile)), true},
		{`{"user":null}`, "", false}, {`{"user":42}`, "", false}, {`[]`, "", false},
	} {
		if err := os.WriteFile(filepath.Join(profile, "session.json"), []byte(tc.session), 0600); err != nil {
			t.Fatal(err)
		}
		account, err := startupProfileAccount(profile)
		if tc.valid && (err != nil || account != tc.account) || !tc.valid && err == nil {
			t.Fatalf("account projection: %q, %v", account, err)
		}
	}
}

func TestStartupScopePaths(t *testing.T) {
	root, err := canonicalSessionDirectory(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	missing := filepath.Join(root, "missing", "profile")
	if path, err := startupScopePath(missing); err != nil || !samePath(path, missing) {
		t.Fatalf("cold profile path: %q, %v", path, err)
	}
	if _, err := startupScopePath(root + string(filepath.Separator) + ".." + string(filepath.Separator) + "other"); err == nil {
		t.Fatal("parent traversal accepted")
	}
	if runtime.GOOS == "darwin" {
		for _, alias := range []string{"/var", "/tmp", "/etc"} {
			path := filepath.Join(alias, "synthetic-startup-scope")
			if actual, err := startupScopePath(path); err != nil || actual != "/private"+path {
				t.Fatalf("system alias: %q, %v", actual, err)
			}
			if _, err := startupScopePath(alias); err == nil {
				t.Fatal("final symlink accepted")
			}
		}
	}
}

func TestStartupControlFrames(t *testing.T) {
	for _, raw := range [][]byte{
		{0, 0, 0, 0}, binary.BigEndian.AppendUint32(nil, startupFrameLimit+1), {0, 0, 0, 4, '{', '}'},
		append([]byte{0, 0, 0, 1}, 0xff),
	} {
		var held startupHeld
		if readStartupFrame(bytes.NewReader(raw), &held) == nil {
			t.Fatal("invalid frame accepted")
		}
	}
	for _, raw := range []string{`{"schema_version":1,"unknown":true}`, `{} {}`} {
		frame := append(binary.BigEndian.AppendUint32(nil, uint32(len(raw))), raw...)
		var held startupHeld
		if readStartupFrame(bytes.NewReader(frame), &held) == nil {
			t.Fatal("invalid JSON accepted")
		}
	}
	if err := writeStartupFrame(io.Discard, strings.Repeat("x", startupFrameLimit)); err == nil {
		t.Fatal("oversized frame written")
	}
}

func TestStartupControlLifecycle(t *testing.T) {
	m := standaloneTestManager(t)
	info := standaloneTestSelection(t, m)
	t.Setenv("RSRS_RUNTIME_BOOT_TOKEN", "synthetic-token-not-a-credential")
	t.Setenv("RSRS_SUPER", "synthetic-secret-not-a-credential")
	for _, existing := range []bool{false, true} {
		if existing {
			profile := filepath.Join(m.DataRoot, "accounts", "synthetic")
			if err := os.MkdirAll(profile, 0700); err != nil {
				t.Fatal(err)
			}
			if err := writeJSON(filepath.Join(profile, "session.json"), map[string]string{"user": "synthetic"}); err != nil {
				t.Fatal(err)
			}
			if err := writeJSON(filepath.Join(m.DataRoot, "client.json"), map[string]string{"data_dir": profile}); err != nil {
				t.Fatal(err)
			}
		}
		for _, mode := range []string{"ok", "bad-release", "bad-endpoint", "trailing", "exit-failure", "finish-wait"} {
			t.Run(mode+map[bool]string{true: "-account", false: "-cold"}[existing], func(t *testing.T) {
				t.Setenv("RESPIRE_TEST_STARTUP_CONTROL", mode)
				ctx := startupTestContext(t)
				guard, err := m.acquireStartup(ctx, info)
				if err != nil {
					t.Fatal(err)
				}
				defer guard.Close()
				held := guard.Held()
				if held.AccountID == "" || held.HelperPID == 0 {
					t.Fatal("empty held identity")
				}
				endpoint := &startupEndpoint{42, "synthetic-birth", "synthetic-image", strings.Repeat("a", 64),
					"ca779cd5-a879-4f15-a43c-65eb2dbd87ed", held.AccountID, 1, m.Endpoint}
				if mode == "ok" && !existing {
					endpoint = nil
				}
				released, err := guard.Finish(endpoint)
				if mode == "ok" {
					if err != nil || !reflect.DeepEqual(released.Endpoint, endpoint) {
						t.Fatalf("finish: %+v, %v", released, err)
					}
				} else if err == nil || released != (startupReleased{}) {
					t.Fatalf("failed receipt accepted: %+v, %v", released, err)
				}
				requireStartupReaped(t, guard)
				if _, err := guard.Finish(nil); err == nil {
					t.Fatal("finished helper reused")
				}
			})
		}
	}
}

func TestStartupControlRejectsHeldIdentity(t *testing.T) {
	m := standaloneTestManager(t)
	info := standaloneTestSelection(t, m)
	for _, mode := range []string{"bad-pid", "bad-birth", "bad-profile", "bad-account", "bad-root", "bad-schema", "bad-guard"} {
		t.Run(mode, func(t *testing.T) {
			t.Setenv("RESPIRE_TEST_STARTUP_CONTROL", mode)
			guard, err := m.acquireStartup(startupTestContext(t), info)
			if err == nil || guard != nil {
				t.Fatalf("mismatched held identity admitted: %v", err)
			}
			requireFixtureHelperExited(t, m)
		})
	}
	t.Run("cancel acquisition", func(t *testing.T) {
		t.Setenv("RESPIRE_TEST_STARTUP_CONTROL", "wait")
		ctx, cancel := context.WithTimeout(t.Context(), time.Second)
		defer cancel()
		guard, err := m.acquireStartup(ctx, info)
		if !errors.Is(err, context.DeadlineExceeded) || guard != nil {
			t.Fatalf("acquisition cancellation lost: %v", err)
		}
		requireFixtureHelperExited(t, m)
	})
	t.Run("helper exits while held", func(t *testing.T) {
		t.Setenv("RESPIRE_TEST_STARTUP_CONTROL", "ok")
		guard, err := m.acquireStartup(startupTestContext(t), info)
		if err != nil {
			t.Fatal(err)
		}
		defer guard.Close()
		if err := guard.cmd.Process.Kill(); err != nil {
			t.Fatal(err)
		}
		select {
		case <-guard.workCtx.Done():
		case <-time.After(time.Second):
			t.Fatal("helper death did not cancel startup")
		}
		if _, err := guard.Finish(nil); err == nil {
			t.Fatal("dead helper completed startup")
		}
		requireStartupReaped(t, guard)
	})
	t.Run("cancel held", func(t *testing.T) {
		t.Setenv("RESPIRE_TEST_STARTUP_CONTROL", "ok")
		ctx, cancel := context.WithCancel(startupTestContext(t))
		guard, err := m.acquireStartup(ctx, info)
		if err != nil {
			t.Fatal(err)
		}
		cancel()
		if _, err := guard.Finish(nil); !errors.Is(err, context.Canceled) {
			t.Fatalf("cancellation lost: %v", err)
		}
		requireStartupReaped(t, guard)
	})
}

func startupFixtureHealth(t *testing.T, m *Manager) {
	t.Helper()
	refused := syscall.ECONNREFUSED
	if runtime.GOOS == "windows" {
		refused = syscall.Errno(10061)
	}
	m.localClient.Transport = cliMetadataTransport(func(r *http.Request) (*http.Response, error) {
		raw, err := os.ReadFile(filepath.Join(m.DataRoot, "fixture-health.json"))
		if errors.Is(err, os.ErrNotExist) {
			return nil, &net.OpError{Op: "dial", Net: "tcp", Err: refused}
		}
		if err != nil {
			return nil, err
		}
		return &http.Response{StatusCode: 200, Body: io.NopCloser(bytes.NewReader(raw)), Header: make(http.Header)}, nil
	})
}

func TestStartupProductionWiring(t *testing.T) {
	m := standaloneTestManager(t)
	info := standaloneTestSelection(t, m)
	t.Setenv("RESPIRE_TEST_STARTUP_CONTROL", "ok")
	startupFixtureHealth(t, m)
	health, err := m.startDetached(startupTestContext(t), info)
	if err != nil {
		t.Fatal(err)
	}
	t.Cleanup(func() {
		if err := os.WriteFile(filepath.Join(m.DataRoot, "fixture-stop"), nil, 0600); err != nil {
			t.Error(err)
			return
		}
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		for {
			birth, err := runtimeControlProcessBirth(ctx, int(health.PID))
			if err != nil {
				t.Error(err)
				return
			}
			if birth == "" {
				return
			}
			select {
			case <-ctx.Done():
				t.Error("fixture serving child retained")
				return
			case <-time.After(10 * time.Millisecond):
			}
		}
	})
	if health.PID == 0 || m.directRuntime != health {
		t.Fatal("startup did not publish its completed owner")
	}
	requireFixtureHelperExited(t, m)
	reused, err := m.startDetached(startupTestContext(t), info)
	if err != nil || reused != health {
		t.Fatalf("verified reuse changed owner: %+v, %v", reused, err)
	}
	requireFixtureHelperExited(t, m)
}

func TestStartupDoesNotRetireTerminalMaintenance(t *testing.T) {
	m := standaloneTestManager(t)
	info := standaloneTestSelection(t, m)
	t.Setenv("RESPIRE_TEST_STARTUP_CONTROL", "ok")
	startupFixtureHealth(t, m)
	root, err := canonicalSessionDirectory(m.DataRoot)
	if err != nil {
		t.Fatal(err)
	}
	directory := filepath.Join(root, "runtime", "autostart")
	results := filepath.Join(directory, "account-results")
	if err := os.MkdirAll(results, 0700); err != nil {
		t.Fatal(err)
	}
	operation := "ca779cd5-a879-4f15-a43c-65eb2dbd87ed"
	marker := []byte(`{"phase":"Paused"}`)
	route, err := json.Marshal(runtimeAccountRoute{1, operation})
	if err != nil {
		t.Fatal(err)
	}
	record := []byte(`{}`)
	files := map[string][]byte{"maintenance.json": marker, "account-active.json": route,
		filepath.Join("account-results", operation+".json"): record}
	for name, raw := range files {
		if err := os.WriteFile(filepath.Join(directory, name), raw, 0600); err != nil {
			t.Fatal(err)
		}
	}
	custody := map[string]any{"operation_id": operation, "root": root, "rolled_back": false, "slots_removed": true, "retired": true,
		"client2_terminal_maintenance": runtimeTerminalMaintenance{1, controlDigest(marker), controlDigest(route), controlDigest(record)}}
	if err := writeJSON(filepath.Join(directory, "account-login-"+operation+".json"), custody); err != nil {
		t.Fatal(err)
	}
	if _, err := m.startDetached(startupTestContext(t), info); err == nil || !strings.Contains(err.Error(), "cannot retire account maintenance") {
		t.Fatalf("startup retired maintenance: %v", err)
	}
	for name, raw := range files {
		current, err := os.ReadFile(filepath.Join(directory, name))
		if err != nil || !bytes.Equal(current, raw) {
			t.Fatalf("maintenance changed: %s, %v", name, err)
		}
	}
	if _, err := os.Stat(filepath.Join(m.DataRoot, "fixture-health.json")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("maintenance startup launched serving: %v", err)
	}
	requireFixtureHelperExited(t, m)
}

func TestStartupStaleEndpointRequiresLibraryAuthority(t *testing.T) {
	m := standaloneTestManager(t)
	info := standaloneTestSelection(t, m)
	t.Setenv("RESPIRE_TEST_STARTUP_CONTROL", "ok")
	startupFixtureHealth(t, m)
	path := filepath.Join(m.DataRoot, "runtime", "endpoint.json")
	if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
		t.Fatal(err)
	}
	original := []byte(`{"historical":"synthetic-only"}`)
	if err := os.WriteFile(path, original, 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := m.startDetached(startupTestContext(t), info); !errors.Is(err, errRuntimeHostAuthorityUnavailable) {
		t.Fatalf("stale endpoint bypassed library authority: %v", err)
	}
	current, err := os.ReadFile(path)
	if err != nil || !bytes.Equal(current, original) {
		t.Fatalf("historical endpoint changed: %v", err)
	}
	if _, err := os.Stat(filepath.Join(m.DataRoot, "fixture-health.json")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("stale endpoint launched a child: %v", err)
	}
	requireFixtureHelperExited(t, m)
}

func TestStartupCancellationCleansServingChild(t *testing.T) {
	m := standaloneTestManager(t)
	info := standaloneTestSelection(t, m)
	t.Setenv("RESPIRE_TEST_STARTUP_CONTROL", "ok")
	t.Setenv("RESPIRE_TEST_STARTUP_PENDING", "1")
	startupFixtureHealth(t, m)
	ctx, cancel := context.WithTimeout(t.Context(), time.Second)
	defer cancel()
	if _, err := m.startDetached(ctx, info); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("startup cancellation lost: %v", err)
	}
	requireFixtureServingExited(t, m)
	requireFixtureHelperExited(t, m)
}

func TestStartupProductionWiringFailureCleansServingChild(t *testing.T) {
	m := standaloneTestManager(t)
	info := standaloneTestSelection(t, m)
	t.Setenv("RESPIRE_TEST_STARTUP_CONTROL", "bad-release")
	startupFixtureHealth(t, m)
	if health, err := m.startDetached(startupTestContext(t), info); err == nil || health != (HealthInfo{}) {
		t.Fatalf("failed startup returned success: %+v, %v", health, err)
	}
	requireFixtureServingExited(t, m)
}

func requireFixtureServingExited(t *testing.T, m *Manager) {
	t.Helper()
	raw, err := os.ReadFile(filepath.Join(m.DataRoot, "fixture-health.json"))
	if err != nil {
		t.Fatal(err)
	}
	var child HealthInfo
	if err := json.Unmarshal(raw, &child); err != nil {
		t.Fatal(err)
	}
	birth, err := runtimeControlProcessBirth(startupTestContext(t), int(child.PID))
	if err != nil || birth != "" || m.directRuntime != (HealthInfo{}) {
		t.Fatalf("failed child retained: %q, %v", birth, err)
	}
}
