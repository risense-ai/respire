package host

import (
	"errors"
	"os"
	"path/filepath"
	"runtime"
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
		if runtime.GOOS == "windows" && errors.Is(err, os.ErrPermission) {
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
