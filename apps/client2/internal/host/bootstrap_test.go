package host

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"respire-client2/internal/i18n"
	"runtime"
	"strings"
	"testing"
)

// buildRuntimeTree materializes a minimal but genuinely verifiable managed
// Runtime tree (binary + notices + core-runtime.json listing both) under dir.
func buildRuntimeTree(t *testing.T, dir, target string) (binaryHash string) {
	t.Helper()
	if err := os.MkdirAll(filepath.Join(dir, "core-notices"), 0700); err != nil {
		t.Fatal(err)
	}
	// The runtime binary must be a valid image for the architecture check,
	// so use a copy of the test binary itself under the triple-qualified
	// archive name the release pipeline produces.
	testBin, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	b, err := os.ReadFile(testBin)
	if err != nil {
		t.Fatal(err)
	}
	target, targetErr := runtimeTarget()
	if targetErr != nil {
		t.Fatal(targetErr)
	}
	binary := filepath.Join(dir, "rsrs-runtime-"+target)
	if err := os.WriteFile(binary, b, 0700); err != nil {
		t.Fatal(err)
	}
	notices := filepath.Join(dir, "core-notices", "NOTICES.txt")
	if err := os.WriteFile(notices, []byte("runtime notices\n"), 0600); err != nil {
		t.Fatal(err)
	}
	noticesHash, err := fileHash(notices)
	if err != nil {
		t.Fatal(err)
	}
	binaryHash, err = fileHash(binary)
	if err != nil {
		t.Fatal(err)
	}
	manifest := map[string]any{
		"schema_version": 1,
		"target":         target,
		"files": []map[string]string{
			{"path": "core-notices/NOTICES.txt", "sha256": noticesHash},
		},
	}
	b, err = json.Marshal(manifest)
	if err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(filepath.Join(dir, "core-runtime.json"), append(b, '\n'), 0600); err != nil {
		t.Fatal(err)
	}
	return binaryHash
}

// buildPayload assembles a complete bootstrap payload fixture and points
// discovery at it. The CLI slot uses the test binary itself so the
// architecture check genuinely passes on the host running the test.
func buildPayload(t *testing.T, cliVersion string) string {
	t.Helper()
	target, err := runtimeTarget()
	if err != nil {
		t.Fatal(err)
	}
	dir := t.TempDir()
	testBin, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	cli := filepath.Join(dir, cliBinary())
	b, err := os.ReadFile(testBin)
	if err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(cli, b, 0700); err != nil {
		t.Fatal(err)
	}
	cliHash, err := fileHash(cli)
	if err != nil {
		t.Fatal(err)
	}
	runtimeHash := buildRuntimeTree(t, filepath.Join(dir, bootstrapRuntimeDir), target)
	// Mirror the build script: the downloaded binary is placed next to the
	// archive tree as rsrs-runtime, which verifyRuntime checks first.
	alias := filepath.Join(dir, bootstrapRuntimeDir, runtimeBinary())
	if err := os.WriteFile(alias, b, 0700); err != nil {
		t.Fatal(err)
	}
	manifest := map[string]any{
		"schema_version": 1,
		"target":         target,
		"channel":        "dev",
		"cli":            map[string]string{"version": cliVersion, "sha256": cliHash},
		"runtime": map[string]string{
			"version": "1.0.21", "sha256": runtimeHash,
			"min_cli": "0.4.0", "max_cli": "*",
		},
	}
	b, err = json.Marshal(manifest)
	if err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(filepath.Join(dir, bootstrapManifestFile), append(b, '\n'), 0600); err != nil {
		t.Fatal(err)
	}
	meta := RuntimeMetadata{Schema: 2, RuntimeVersion: "1.0.21", Tag: "v1.0.21", Target: target,
		ArchiveFile: runtimeArchive(target), ArchiveHash: strings.Repeat("0", 64), Binary: "rsrs-runtime-" + target + binaryExtension(), BinaryHash: runtimeHash,
		Compat: Compat{MinCLI: "0.4.0", MaxCLI: "*"}}
	metadata, err := fixtureReleaseJSON(t, filepath.Join(dir, bootstrapRuntimeDir), meta)
	if err != nil {
		t.Fatal(err)
	}
	if err = os.WriteFile(filepath.Join(dir, "runtime-build-"+target+".json"), metadata, 0600); err != nil {
		t.Fatal(err)
	}
	t.Setenv(bootstrapEnvOverride, dir)
	return dir
}

func fixtureReleaseJSON(t *testing.T, tree string, meta RuntimeMetadata) ([]byte, error) {
	t.Helper()
	binary := filepath.Join(tree, "rsrs-runtime-"+meta.Target)
	var files []map[string]any
	for _, entry := range []struct{ name, path string }{
		{meta.Binary, binary}, {"core-runtime.json", filepath.Join(tree, "core-runtime.json")},
		{"core-notices/NOTICES.txt", filepath.Join(tree, "core-notices", "NOTICES.txt")},
	} {
		info, err := os.Stat(entry.path)
		if err != nil {
			return nil, err
		}
		hash, err := fileHash(entry.path)
		if err != nil {
			return nil, err
		}
		files = append(files, map[string]any{"path": entry.name, "size": info.Size(), "sha256": hash})
	}
	b, err := json.Marshal(meta)
	if err != nil {
		return nil, err
	}
	var fields map[string]any
	if err = json.Unmarshal(b, &fields); err != nil {
		return nil, err
	}
	fields["files"], fields["source_commit"] = files, strings.Repeat("0", 40)
	fields["protocol_range"] = map[string]int{"minimum": 1, "maximum": 1}
	return json.Marshal(fields)
}

func TestBootstrapPayloadDiscovery(t *testing.T) {
	t.Run("missing payload keeps the online path", func(t *testing.T) {
		t.Setenv(bootstrapEnvOverride, filepath.Join(t.TempDir(), "absent"))
		if _, err := readBootstrapPayload(); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("absent payload must report ErrNotExist, got %v", err)
		}
		if status := newInstallTestManager(t).BootstrapStatus(context.Background()); status.PayloadError != "" {
			t.Fatalf("absent payload must not report an error: %+v", status)
		}
	})
	t.Run("valid payload verifies in full", func(t *testing.T) {
		dir := buildPayload(t, "0.4.1")
		payload, err := readBootstrapPayload()
		if err != nil {
			t.Fatalf("valid payload must verify: %v", err)
		}
		if payload.Dir != dir || payload.Manifest.CLI.Version != "0.4.1" || payload.Manifest.Runtime.Version != "1.0.21" {
			t.Fatalf("payload identity drifted: %+v", payload.Manifest)
		}
	})
	t.Run("foreign-platform payload is rejected", func(t *testing.T) {
		dir := buildPayload(t, "0.4.1")
		b, err := os.ReadFile(filepath.Join(dir, bootstrapManifestFile))
		if err != nil {
			t.Fatal(err)
		}
		var manifest map[string]any
		if err := json.Unmarshal(b, &manifest); err != nil {
			t.Fatal(err)
		}
		if manifest["target"] == "x86_64-unknown-linux-gnu" {
			manifest["target"] = "aarch64-apple-darwin"
		} else {
			manifest["target"] = "x86_64-unknown-linux-gnu"
		}
		b, _ = json.Marshal(manifest)
		if err := os.WriteFile(filepath.Join(dir, bootstrapManifestFile), b, 0600); err != nil {
			t.Fatal(err)
		}
		if _, err := readBootstrapPayload(); err == nil || errors.Is(err, os.ErrNotExist) {
			t.Fatalf("foreign platform must reject with a specific error, got %v", err)
		}
	})
	t.Run("tampered payload is rejected", func(t *testing.T) {
		dir := buildPayload(t, "0.4.1")
		// CLI tamper: rewrite bytes, keep the manifest hash.
		if err := os.WriteFile(filepath.Join(dir, cliBinary()), []byte("evil"), 0700); err != nil {
			t.Fatal(err)
		}
		if _, err := readBootstrapPayload(); err == nil {
			t.Fatal("tampered CLI must be rejected")
		}
		// Runtime tamper: flip bytes inside a manifest-listed file.
		dir = buildPayload(t, "0.4.1")
		notices := filepath.Join(dir, bootstrapRuntimeDir, "core-notices", "NOTICES.txt")
		if err := os.WriteFile(notices, []byte("tampered notices\n"), 0600); err != nil {
			t.Fatal(err)
		}
		if _, err := readBootstrapPayload(); err == nil {
			t.Fatal("tampered Runtime tree must be rejected")
		}
	})
	t.Run("unknown manifest fields reject", func(t *testing.T) {
		dir := buildPayload(t, "0.4.1")
		b, err := os.ReadFile(filepath.Join(dir, bootstrapManifestFile))
		if err != nil {
			t.Fatal(err)
		}
		var manifest map[string]any
		if err := json.Unmarshal(b, &manifest); err != nil {
			t.Fatal(err)
		}
		manifest["surprise"] = true
		b, _ = json.Marshal(manifest)
		if err := os.WriteFile(filepath.Join(dir, bootstrapManifestFile), b, 0600); err != nil {
			t.Fatal(err)
		}
		if _, err := readBootstrapPayload(); err == nil {
			t.Fatal("unknown manifest fields must reject the payload")
		}
	})
}

// cliProbeFixture controls the version probe during CLI install tests: the
// staging copy (matched by its .rsrs-payload- temp prefix) and the installed
// CLI each report a version or fail as "unparseable".
type cliProbeFixture struct {
	tmpVersion string
	tmpBroken  bool
	destAbsent bool // no probe answer for the installed CLI
	destBroken bool
	destVer    string
}

func stubCLIProbe(t *testing.T, f cliProbeFixture) {
	t.Helper()
	original := probeCLIVersion
	probeCLIVersion = func(_ context.Context, path string) (string, error) {
		if strings.Contains(filepath.Base(path), ".rsrs-payload-") {
			if f.tmpBroken {
				return "", errors.New(i18n.Messagee3b08dc9b1d7d01d)
			}
			return f.tmpVersion, nil
		}
		if f.destAbsent {
			return "", fmt.Errorf("no probe fixture for %s", path)
		}
		if f.destBroken {
			return "", errors.New("unparseable output")
		}
		return f.destVer, nil
	}
	t.Cleanup(func() { probeCLIVersion = original })
}

func newInstallTestManager(t *testing.T) *Manager {
	t.Helper()
	// The runner's temporary directory can use an alias for the actual volume path.
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	m := &Manager{Root: filepath.Join(root, "client2"), DataRoot: root,
		Endpoint: "http://127.0.0.1:1", HTTPClient: &http.Client{}, prefs: defaultPreferences(),
		localClient: &http.Client{Transport: &http.Transport{Proxy: nil}}}
	if err := os.MkdirAll(m.Root, 0700); err != nil {
		t.Fatal(err)
	}
	return m
}

func TestInstallBootstrapCLIAtomicPlacement(t *testing.T) {
	t.Run("fresh install publishes the payload CLI", func(t *testing.T) {
		buildPayload(t, "1.2.3")
		payload, err := readBootstrapPayload()
		if err != nil {
			t.Fatal(err)
		}
		m := newInstallTestManager(t)
		stubCLIProbe(t, cliProbeFixture{tmpVersion: "1.2.3", destAbsent: true})
		if err = m.installBootstrapCLI(context.Background(), &payload); err != nil {
			t.Fatalf("fresh install failed: %v", err)
		}
		dest := filepath.Join(m.DataRoot, "bin", cliBinary())
		info, err := os.Stat(dest)
		if err != nil {
			t.Fatalf("CLI not installed: %v", err)
		}
		if runtime.GOOS != "windows" && info.Mode().Perm() != 0700 {
			t.Fatalf("installed CLI must be 0700, got %v", info.Mode().Perm())
		}
		if hash, err := fileHash(dest); err != nil || hash != payload.Manifest.CLI.SHA256 {
			t.Fatalf("installed CLI bytes wrong: %v %s", err, hash)
		}
		entries, _ := os.ReadDir(filepath.Dir(dest))
		if len(entries) != 1 {
			t.Fatalf("staging leftovers in bin dir: %v", entries)
		}
	})
	t.Run("identical installed CLI is reused while in use", func(t *testing.T) {
		buildPayload(t, "1.2.3")
		payload, err := readBootstrapPayload()
		if err != nil {
			t.Fatal(err)
		}
		m := newInstallTestManager(t)
		source := filepath.Join(payload.Dir, cliBinary())
		bin, err := os.ReadFile(source)
		if err != nil {
			t.Fatal(err)
		}
		dest := filepath.Join(m.DataRoot, "bin", cliBinary())
		if err := os.MkdirAll(filepath.Dir(dest), 0700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(dest, bin, 0700); err != nil {
			t.Fatal(err)
		}
		held, err := os.Open(dest)
		if err != nil {
			t.Fatal(err)
		}
		defer held.Close()
		before, err := held.Stat()
		if err != nil {
			t.Fatal(err)
		}
		stubCLIProbe(t, cliProbeFixture{destVer: "1.2.3", tmpBroken: true})
		if err := m.installBootstrapCLI(context.Background(), &payload); err != nil {
			t.Fatalf("identical installed CLI must be reusable: %v", err)
		}
		after, err := os.Stat(dest)
		if err != nil {
			t.Fatal(err)
		}
		if !os.SameFile(before, after) {
			t.Fatal("identical installed CLI was replaced")
		}
	})
	t.Run("equal version with different bytes refreshes from the verified payload", func(t *testing.T) {
		payload, err := func() (bootstrapPayload, error) {
			buildPayload(t, "1.2.3")
			return readBootstrapPayload()
		}()
		if err != nil {
			t.Fatal(err)
		}
		m := newInstallTestManager(t)
		dest := filepath.Join(m.DataRoot, "bin", cliBinary())
		if err = os.MkdirAll(filepath.Dir(dest), 0700); err != nil {
			t.Fatal(err)
		}
		if err = os.WriteFile(dest, []byte("same version but corrupt bytes"), 0700); err != nil {
			t.Fatal(err)
		}
		stubCLIProbe(t, cliProbeFixture{tmpVersion: "1.2.3", destVer: "1.2.3"})
		if err = m.installBootstrapCLI(context.Background(), &payload); err != nil {
			t.Fatalf("equal-version reinstall failed: %v", err)
		}
		if hash, _ := fileHash(dest); hash != payload.Manifest.CLI.SHA256 {
			t.Fatal("equal version must still refresh to the verified payload bytes")
		}
	})
	t.Run("older version upgrades", func(t *testing.T) {
		payload, err := func() (bootstrapPayload, error) {
			buildPayload(t, "1.2.3")
			return readBootstrapPayload()
		}()
		if err != nil {
			t.Fatal(err)
		}
		m := newInstallTestManager(t)
		dest := filepath.Join(m.DataRoot, "bin", cliBinary())
		if err = os.MkdirAll(filepath.Dir(dest), 0700); err != nil {
			t.Fatal(err)
		}
		if err = os.WriteFile(dest, []byte("old"), 0700); err != nil {
			t.Fatal(err)
		}
		stubCLIProbe(t, cliProbeFixture{tmpVersion: "1.2.3", destVer: "1.0.0"})
		if err = m.installBootstrapCLI(context.Background(), &payload); err != nil {
			t.Fatalf("upgrade failed: %v", err)
		}
		if hash, _ := fileHash(dest); hash != payload.Manifest.CLI.SHA256 {
			t.Fatal("older CLI must be upgraded to the payload version")
		}
	})
	t.Run("newer version is kept untouched", func(t *testing.T) {
		payload, err := func() (bootstrapPayload, error) {
			buildPayload(t, "1.2.3")
			return readBootstrapPayload()
		}()
		if err != nil {
			t.Fatal(err)
		}
		m := newInstallTestManager(t)
		dest := filepath.Join(m.DataRoot, "bin", cliBinary())
		if err = os.MkdirAll(filepath.Dir(dest), 0700); err != nil {
			t.Fatal(err)
		}
		if err = os.WriteFile(dest, []byte("newer-installed-cli"), 0700); err != nil {
			t.Fatal(err)
		}
		stubCLIProbe(t, cliProbeFixture{tmpVersion: "1.2.3", destVer: "9.9.9"})
		if err = m.installBootstrapCLI(context.Background(), &payload); err != nil {
			t.Fatalf("newer CLI must be kept without error: %v", err)
		}
		if b, _ := os.ReadFile(dest); string(b) != "newer-installed-cli" {
			t.Fatal("a newer installed CLI must never be overwritten")
		}
	})
	t.Run("verified CLI repairs an old file with an unreadable version", func(t *testing.T) {
		payload, err := func() (bootstrapPayload, error) {
			buildPayload(t, "1.2.3")
			return readBootstrapPayload()
		}()
		if err != nil {
			t.Fatal(err)
		}
		m := newInstallTestManager(t)
		dest := filepath.Join(m.DataRoot, "bin", cliBinary())
		if err = os.MkdirAll(filepath.Dir(dest), 0700); err != nil {
			t.Fatal(err)
		}
		if err = os.WriteFile(dest, []byte("mystery-file"), 0700); err != nil {
			t.Fatal(err)
		}
		stubCLIProbe(t, cliProbeFixture{tmpVersion: "1.2.3", destBroken: true})
		err = m.installBootstrapCLI(context.Background(), &payload)
		if err != nil {
			t.Fatalf("verified CLI did not replace an unreadable old version: %v", err)
		}
		if got, err := fileHash(dest); err != nil || got != payload.Manifest.CLI.SHA256 {
			t.Fatalf("verified CLI was not published: %s %v", got, err)
		}
	})
	t.Run("failed staging probe leaves the existing CLI in place", func(t *testing.T) {
		payload, err := func() (bootstrapPayload, error) {
			buildPayload(t, "1.2.3")
			return readBootstrapPayload()
		}()
		if err != nil {
			t.Fatal(err)
		}
		m := newInstallTestManager(t)
		dest := filepath.Join(m.DataRoot, "bin", cliBinary())
		if err = os.MkdirAll(filepath.Dir(dest), 0700); err != nil {
			t.Fatal(err)
		}
		if err = os.WriteFile(dest, []byte("old"), 0700); err != nil {
			t.Fatal(err)
		}
		stubCLIProbe(t, cliProbeFixture{tmpBroken: true, destVer: "1.0.0"})
		if err = m.installBootstrapCLI(context.Background(), &payload); err == nil {
			t.Fatal("a failing staging probe must abort the install")
		}
		if b, _ := os.ReadFile(dest); string(b) != "old" {
			t.Fatal("existing CLI must survive a failed install attempt")
		}
		entries, _ := os.ReadDir(filepath.Dir(dest))
		if len(entries) != 1 {
			t.Fatalf("staging leftovers after failure: %v", entries)
		}
	})
}
