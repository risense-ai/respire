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
[IO.Directory]::CreateDirectory((Join-Path $root 'physical\replacement')) | Out-Null
New-Item -ItemType Junction -Path (Join-Path $root 'selected') -Target (Join-Path $root 'physical') | Out-Null
foreach ($child in 'physical', 'physical\profile', 'physical\replacement', 'selected') {
    & (Join-Path $env:SystemRoot 'System32\icacls.exe') (Join-Path $root $child) /setowner ('*' + $user.Value) /L | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Could not set the synthetic directory owner' }
}`
	system := os.Getenv("SystemRoot")
	cmd := exec.Command(filepath.Join(system, "System32", "WindowsPowerShell", "v1.0", "powershell.exe"),
		"-NoProfile", "-NonInteractive", "-Command", script)
	cmd.Env = []string{"PATHEXT=.EXE", "SystemRoot=" + system, "WINDIR=" + system, "RESPIRE_PROFILE_FIXTURE=" + root}
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
	if err := os.Rename(filepath.Join(root, "physical", "replacement"), want); err != nil {
		t.Fatal(err)
	}
	replacement, err := pinConfiguredProfile(selected)
	if err != nil {
		t.Fatalf("replacement should pass standalone owner checks: %v", err)
	}
	replacement.close()
	if _, err := m.installModelEngines(t.Context()); !errors.Is(err, errJunctionTakeover) {
		t.Fatalf("junction provider install reached catalog download: %v", err)
	}
	checkReplacedProfileControls(t, m)
}

func TestNewRefusesMissingProfileBelowOwnedJunction(t *testing.T) {
	root := t.TempDir()
	selected := syntheticJunctionProfile(t, root)
	if err := os.Remove(filepath.Join(root, "physical", "profile")); err != nil {
		t.Fatal(err)
	}
	t.Setenv("RSRS_DATA_DIR", selected)
	t.Setenv("RSRS_RPC_PORT", "15169")
	if _, err := New(); err == nil {
		t.Fatal("missing profile beneath a junction was admitted")
	}
	if _, err := os.Stat(selected); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("unresolved profile touched: %v", err)
	}
}


func TestNewRefusesJunctionAsProfileLeaf(t *testing.T) {
	selected := syntheticJunctionProfile(t, t.TempDir())
	t.Setenv("RSRS_DATA_DIR", filepath.Dir(selected))
	t.Setenv("RSRS_RPC_PORT", "15169")
	if _, err := New(); err == nil {
		t.Fatal("junction itself was accepted as the configured profile")
	}
}

func TestRequestLedgerRefusesReplacedClientDirectory(t *testing.T) {
	root := t.TempDir()
	t.Setenv("RSRS_DATA_DIR", syntheticJunctionProfile(t, root))
	m, err := New()
	if err != nil {
		t.Fatal(err)
	}
	if err := os.Rename(m.Root, filepath.Join(m.DataRoot, "original-client2")); err != nil {
		t.Fatal(err)
	}
	target := filepath.Join(root, "physical", "replacement")
	system := os.Getenv("SystemRoot")
	cmd := exec.Command(filepath.Join(system, "System32", "WindowsPowerShell", "v1.0", "powershell.exe"),
		"-NoProfile", "-NonInteractive", "-Command",
		"$ErrorActionPreference='Stop'; New-Item -ItemType Junction -Path $env:RESPIRE_LINK -Target $env:RESPIRE_TARGET | Out-Null")
	cmd.Env = []string{"PATHEXT=.EXE", "SystemRoot=" + system, "WINDIR=" + system, "RESPIRE_LINK=" + m.Root, "RESPIRE_TARGET=" + target}
	if out, err := cmd.CombinedOutput(); err != nil {
		t.Fatalf("create synthetic nested junction: %v: %s", err, out)
	}
	t.Cleanup(func() { os.Remove(m.Root) })
	if err := m.persistOperationsLocked(); !errors.Is(err, errProfileScope) {
		t.Fatalf("ledger accepted replaced client directory: %v", err)
	}
	if entries, err := os.ReadDir(target); err != nil || len(entries) != 0 {
		t.Fatalf("redirected ledger target changed: %v, %v", entries, err)
	}
}
