package host

import (
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"syscall"
	"testing"
)

func TestNewChecksProfileBeforeCreatingClientDirectory(t *testing.T) {
	root := t.TempDir()
	target := filepath.Join(root, "physical")
	if err := os.Mkdir(target, 0700); err != nil {
		t.Fatal(err)
	}
	alias := filepath.Join(root, "alias")
	if err := os.Symlink(target, alias); err != nil {
		if runtime.GOOS == "windows" && (errors.Is(err, os.ErrPermission) || errors.Is(err, syscall.Errno(1314))) {
			t.Skip("directory symlinks require Windows symlink privileges")
		}
		t.Fatal(err)
	}
	t.Setenv("RSRS_DATA_DIR", filepath.Join(alias, "profile"))
	t.Setenv("RSRS_RPC_PORT", "15169")
	if _, err := New(); err == nil {
		t.Fatal("accepted a directory symlink in the configured root")
	}
	if _, err := os.Lstat(filepath.Join(target, "profile")); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("rejected profile was touched: %v", err)
	}
}

// All serving callers, including already-locked maintenance callers, and both
// native model verbs must stop before reading selections or launching helpers.
func checkReplacedProfileControls(t *testing.T, m *Manager) {
	t.Helper()
	ctx := t.Context()
	if runtime.GOOS == "linux" || runtime.GOOS == "windows" {
		checkJunctionMigrationRejected(t, m)
	}
	if _, err := m.startDetached(ctx, InstalledRuntime{}); !errors.Is(err, errProfileScope) {
		t.Fatalf("startup reached selection: %v", err)
	}
	if _, err := m.startDetachedLocked(ctx, InstalledRuntime{}); !errors.Is(err, errProfileScope) {
		t.Fatalf("locked startup reached selection: %v", err)
	}
	if _, err := m.launchRuntimeServing(ctx, &exec.Cmd{}); !errors.Is(err, errProfileScope) {
		t.Fatalf("direct serving reached launch: %v", err)
	}
	if err := m.installRuntimeOwnedModel(ctx, HealthInfo{}, "m3", strings.NewReader(""), strings.NewReader("")); !errors.Is(err, errProfileScope) {
		t.Fatalf("installation reached native model control: %v", err)
	}
	if _, err := m.removeRuntimeOwnedModel(ctx, HealthInfo{}, "m3"); !errors.Is(err, errProfileScope) {
		t.Fatalf("removal reached native model control: %v", err)
	}
	if _, err := m.uninstallM3Locked(ctx); !errors.Is(err, errProfileScope) {
		t.Fatalf("uninstall reached takeover admission: %v", err)
	}
}

func TestReplacedProfileStopsServingAndModelControls(t *testing.T) {
	root := t.TempDir()
	m := &Manager{DataRoot: root, profile: profileAdmission{root: root, reopen: func() (func(), error) {
		return nil, errProfileScope
	}}}
	checkReplacedProfileControls(t, m)
}

// The authority fixture creates the runtime directory on its first takeover.
// Rejection before that callback excludes the later private helper launch.
func checkJunctionMigrationRejected(t *testing.T, m *Manager) {
	t.Helper()
	const operation = "a463ac20-7783-4077-a62f-e931d6b58466"
	authority := &hostAuthorityFixture{}
	previous := m.hostAuthority
	m.hostAuthority = authority
	defer func() { m.hostAuthority = previous }()
	for _, query := range []bool{false, true} {
		if _, err := m.desktopLegacyMigration(t.Context(), "desktop-"+operation, nil, query); !errors.Is(err, errJunctionTakeover) {
			t.Fatalf("migration query=%t reached transaction admission: %v", query, err)
		}
	}
	if authority.held != nil {
		t.Fatal("migration invoked native takeover before rejection")
	}
	for _, name := range []string{"maintenance.json", "migration-client2-" + operation + ".json"} {
		if _, err := os.Lstat(filepath.Join(m.DataRoot, "runtime", "autostart", name)); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("rejected migration touched %s: %v", name, err)
		}
	}
}
