package host

import (
	"errors"
	"os"
	"os/exec"
	"path/filepath"
	"testing"
)

func syntheticJunctionProfile(t *testing.T, root string) string {
	t.Helper()
	root = windowsDOSPath(root)
	physical := filepath.Join(root, "physical")
	alias := filepath.Join(root, "selected")
	// Set the DACL before creating the tree so descendants inherit it.
	script := `$ErrorActionPreference='Stop'
$root=$env:RESPIRE_PROFILE_FIXTURE
$user=[Security.Principal.WindowsIdentity]::GetCurrent().User
$acl=New-Object Security.AccessControl.DirectorySecurity
$acl.SetOwner($user)
$acl.SetAccessRuleProtection($true,$false)
$acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($user,'FullControl','ContainerInherit,ObjectInherit','None','Allow'))
Set-Acl -LiteralPath $root -AclObject $acl
[IO.Directory]::CreateDirectory((Join-Path $root 'physical\profile')) | Out-Null
New-Item -ItemType Junction -Path (Join-Path $root 'selected') -Target (Join-Path $root 'physical') | Out-Null`
	system := os.Getenv("SystemRoot")
	cmd := exec.Command(filepath.Join(system, "System32", "WindowsPowerShell", "v1.0", "powershell.exe"),
		"-NoProfile", "-NonInteractive", "-Command", script)
	cmd.Env = []string{"SystemRoot="+system, "WINDIR="+system, "RESPIRE_PROFILE_FIXTURE="+root}
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("creating synthetic NTFS junction: %v: %s", err, out)
	}
	t.Cleanup(func() { os.Remove(alias) })
	if _, err := os.Stat(physical); err != nil {
		t.Fatal(err)
	}
	return filepath.Join(alias, "profile")
}

func TestNewBindsConfiguredJunctionToPhysicalProfile(t *testing.T) {
	root := t.TempDir()
	selected := syntheticJunctionProfile(t, root)
	t.Setenv("RSRS_DATA_DIR", selected)
	t.Setenv("RSRS_RPC_PORT", "15169")
	m, err := New()
	if err != nil {
		t.Fatal(err)
	}
	want := filepath.Join(root, "physical", "profile")
	if m.profile.reopen == nil || !samePath(m.DataRoot, want) {
		t.Fatalf("unexpected admitted directory %q", m.DataRoot)
	}
	release, err := m.holdProfile()
	if err != nil {
		t.Fatal(err)
	}
	moved := filepath.Join(root, "physical", "moved")
	err = os.Rename(want, moved)
	release()
	if err == nil {
		t.Fatal("admitted directory moved while held")
	}
	if err := os.Rename(want, moved); err != nil {
		t.Fatal(err)
	}
	if err := os.Mkdir(want, 0700); err != nil {
		t.Fatal(err)
	}
	if _, err := m.removeRuntimeOwnedModel(t.Context(), HealthInfo{}, "m3"); !errors.Is(err, errProfileScope) {
		t.Fatalf("replacement reached model admission: %v", err)
	}
}
