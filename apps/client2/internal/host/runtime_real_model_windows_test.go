package host

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"

	"golang.org/x/sys/windows"
)

func TestManagedModelCapabilityNeverFallsBackOnUncertainProbe(t *testing.T) {
	cases := []struct {
		status          int
		marker          string
		ready           bool
		supported, fail bool
	}{
		{http.StatusNotFound, "", true, false, false},
		{http.StatusMethodNotAllowed, "", true, false, false},
		{http.StatusNoContent, "pinned-pair-v1", true, true, false},
		{http.StatusNoContent, "pinned-pair-v1", false, false, true},
		{http.StatusNoContent, "", true, false, true},
		{http.StatusForbidden, "", true, false, true},
		{http.StatusServiceUnavailable, "", true, false, true},
	}
	for _, test := range cases {
		ok, err := runtimeInstallCapabilityResult(test.status, test.marker, test.ready)
		if ok != test.supported || (err != nil) != test.fail {
			t.Fatalf("unexpected capability result for HTTP %d: %v, %v", test.status, ok, err)
		}
	}
}

func TestStartupProbeRetriesOnlyOwnedChildTransportReset(t *testing.T) {
	reset := &net.OpError{Op: "read", Net: "tcp", Err: windows.WSAECONNRESET}
	if !runtimeStartingProbeRetryable(reset) || IsUnavailable(reset) {
		t.Fatal("an owned-child probe should retry a reset without treating it as absence elsewhere")
	}
	if runtimeStartingProbeRetryable(windows.WSAECONNRESET) ||
		runtimeStartingProbeRetryable(errors.New("Runtime identity mismatch")) {
		t.Fatal("startup retry must not ignore unrelated failures")
	}
}

func TestRealRuntimeOwnedModelUninstallWindows(t *testing.T) {
	source := os.Getenv("RESPIRE_TEST_REAL_MODEL_FIXTURE")
	if source == "" {
		t.Skip("set RESPIRE_TEST_REAL_MODEL_FIXTURE to an isolated directory containing the pinned bge-small assets")
	}
	m3Source := os.Getenv("RESPIRE_TEST_REAL_M3_FIXTURE")
	if !filepath.IsAbs(source) {
		t.Fatal("the explicit model fixture directory must be absolute")
	}
	m, info := realRuntimeManager(t)
	ctx := realRuntimeContext(t)
	t.Cleanup(func() { cleanupRealRuntime(t, m, info) })
	protect := func(path string) {
		t.Helper()
		c, cancel := context.WithTimeout(ctx, 10*time.Second)
		defer cancel()
		// Windows mkdir does not restrict permissions. Set a native owner-only
		// DACL before launching any process over this synthetic test profile.
		const acl = "$ErrorActionPreference='Stop';" +
			"$owner=[Security.Principal.WindowsIdentity]::GetCurrent().User;" +
			"$acl=New-Object Security.AccessControl.DirectorySecurity;" +
			"$acl.SetOwner($owner);$acl.SetAccessRuleProtection($true,$false);" +
			"$acl.AddAccessRule([Security.AccessControl.FileSystemAccessRule]::new($owner,'FullControl','ContainerInherit,ObjectInherit','None','Allow'));" +
			"Set-Acl -LiteralPath $env:RESPIRE_MODEL_TEST_ROOT -AclObject $acl"
		windows := os.Getenv("SystemRoot")
		cmd := exec.CommandContext(c, filepath.Join(windows, "System32", "WindowsPowerShell", "v1.0", "powershell.exe"),
			"-NoProfile", "-NonInteractive", "-Command", acl)
		cmd.Env = []string{"SystemRoot=" + windows, "WINDIR=" + windows, "RESPIRE_MODEL_TEST_ROOT=" + path}
		if output, err := cmd.CombinedOutput(); err != nil {
			t.Fatalf("setting the synthetic directory DACL failed: %v: %s", err, output)
		}
	}
	protect(m.DataRoot)
	profile, err := canonicalSessionDirectory(m.DataRoot)
	if err != nil || !strings.HasPrefix(profile, "\\\\?\\") || len(profile) < 7 || profile[5] != ':' {
		t.Fatal("expected a canonical extended local synthetic DOS path", err)
	}
	// The serving Runtime allocates, fills and publishes its own files. Only
	// the duplicate outside the managed namespace is prepared by the fixture.
	rawProfile := profile
	modelID := "bge-small-zh-v1.5"
	managed := filepath.Join(m.DataRoot, "runtime", "model-assets-v1", controlDigest([]byte(rawProfile)), modelID)
	unmanaged := filepath.Join(m.DataRoot, "external-test-model")
	for _, directory := range []string{filepath.Join(m.DataRoot, "runtime"), unmanaged} {
		if err := os.MkdirAll(directory, 0700); err != nil {
			t.Fatal(err)
		}
	}
	protect(filepath.Join(m.DataRoot, "runtime"))
	files := map[string]string{
		"model_quantized.onnx": "15b717c382bcb518ba457b93ea6850ede7f4f1cd8937454aa06972366cd19bcc",
		"tokenizer.json":       "48cea5d44424912a6fd1ea647bf4fe50b55ab8b1e5879c3275f80e339e8fae26",
	}
	for name, want := range files {
		input, err := os.ReadFile(filepath.Join(source, name))
		if err != nil {
			t.Fatal(err)
		}
		sum := sha256.Sum256(input)
		if hex.EncodeToString(sum[:]) != want {
			t.Fatal("model fixture digest mismatch")
		}
		if err := os.WriteFile(filepath.Join(unmanaged, name), input, 0600); err != nil {
			t.Fatal(err)
		}
	}
	factor, digest := disposableV4Session(t, profile)
	locked, err := m.startDetached(ctx, info)
	if err != nil {
		t.Fatal(err)
	}
	token, err := m.runtimeUnlockToken()
	if err != nil {
		t.Fatal(err)
	}
	realUnlockV2(t, ctx, m, locked, profile, digest, token, factor, http.StatusOK)
	ready, _, err := m.Health(ctx)
	if err != nil || !ready.StoreReady {
		t.Fatalf("synthetic Runtime did not unlock: %v", err)
	}
	credentialPath := filepath.Join(m.DataRoot, "runtime", ".owner-credentials-"+ready.InstanceID)
	for tries := 0; tries < 100; tries++ {
		if info, err := os.Stat(credentialPath); err == nil && info.IsDir() {
			break
		}
		select {
		case <-ctx.Done():
			t.Fatal(ctx.Err())
		case <-time.After(20 * time.Millisecond):
		}
	}
	wrong := ready
	wrong.AccountID = strings.Repeat("1", 64)
	if _, err := m.removeRuntimeOwnedModel(ctx, wrong, modelID); !errors.Is(err, errOnlineModelUnconfirmed) {
		t.Fatalf("private dispatch accepted a different account: %v", err)
	}
	// Exercise the same Manager operation as the desktop action. A refused
	// active-model uninstall must not enter the legacy stop/restart path.
	if _, err := m.uninstallM3(ctx); !errors.Is(err, errM3Active) {
		t.Fatalf("desktop M3 operation did not reject the current implicit selection: %v", err)
	}
	weights, err := os.Open(filepath.Join(source, "model_quantized.onnx"))
	if err != nil {
		t.Fatal(err)
	}
	defer weights.Close()
	tokenizer, err := os.Open(filepath.Join(source, "tokenizer.json"))
	if err != nil {
		t.Fatal(err)
	}
	defer tokenizer.Close()
	if _, err := os.Stat(managed); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("fixture unexpectedly pre-provisioned a managed target: %v", err)
	}
	// Rejection uses the same native transfer and real pins, not a fake
	// receipt. A complete upload with altered bytes must publish nothing.
	first := make([]byte, 1)
	if _, err := io.ReadFull(weights, first); err != nil {
		t.Fatal(err)
	}
	first[0] ^= 1
	bad := io.MultiReader(bytes.NewReader(first), weights)
	if err := m.installRuntimeOwnedModel(ctx, ready, modelID, bad, tokenizer); !errors.Is(err, errOnlineModelUnavailable) {
		t.Fatalf("modified weights were not rejected by the Runtime: %v", err)
	}
	if _, err := os.Stat(managed); !errors.Is(err, os.ErrNotExist) {
		t.Fatalf("invalid transfer published a model: %v", err)
	}
	for _, file := range []*os.File{weights, tokenizer} {
		if _, err := file.Seek(0, io.SeekStart); err != nil {
			t.Fatal(err)
		}
	}
	if err := m.installRuntimeOwnedModel(ctx, ready, modelID, weights, tokenizer); err != nil {
		t.Fatalf("native publish failed: %v", err)
	}
	for name, want := range files {
		if digest, err := fileHash(filepath.Join(managed, name)); err != nil || digest != want {
			t.Fatalf("native publish did not store exact pinned bytes: %s %v", name, err)
		}
	}
	removed, err := m.removeRuntimeOwnedModel(ctx, ready, modelID)
	if err != nil || removed.Deleted != 2 {
		t.Fatalf("native model dispatch did not remove the pinned pair: %v", err)
	}
	for name, want := range files {
		if _, err := os.Stat(filepath.Join(managed, name)); !errors.Is(err, os.ErrNotExist) {
			t.Fatal("managed artifact remains")
		}
		if digest, err := fileHash(filepath.Join(unmanaged, name)); err != nil || digest != want {
			t.Fatal("model control changed the unmanaged copy")
		}
	}
	after, _, err := m.Health(ctx)
	if err != nil || !after.StoreReady || after.PID != ready.PID || after.InstanceID != ready.InstanceID ||
		after.Generation != ready.Generation || after.AccountID != ready.AccountID {
		t.Fatalf("model control replaced or interrupted the native process: %v", err)
	}
	if m3Source != "" {
		verifyNativeM3DownloadAndDesktopUninstall(t, m, ready, weights, tokenizer, m3Source)
	}
}

func verifyNativeM3DownloadAndDesktopUninstall(t *testing.T, m *Manager, ready HealthInfo, weights, tokenizer *os.File, source string) {
	t.Helper()
	if !filepath.IsAbs(source) {
		t.Fatal("the explicit M3 model fixture must be absolute")
	}
	ctx, cancel := context.WithTimeout(t.Context(), 4*time.Minute)
	defer cancel()
	if _, supported, err := m.runtimeModelInstallCapability(ctx); err != nil || !supported {
		t.Fatalf("verified Runtime did not negotiate native install capability: %v", err)
	}
	for _, file := range []*os.File{weights, tokenizer} {
		if _, err := file.Seek(0, io.SeekStart); err != nil {
			t.Fatal(err)
		}
	}
	if err := m.installRuntimeOwnedModel(ctx, ready, "bge-small-zh-v1.5", weights, tokenizer); err != nil {
		t.Fatalf("stage the real alternate model for selection: %v", err)
	}
	scope := &runtimeScope{ready.AccountID, ready.InstanceID, ready.Generation}
	operation := "fixture-select-small"
	choice := `"model":"bge-small-zh-v1.5","backend":"onnx","engine":"cpu","variant":"quantized"`
	if _, err := m.switchManagedModel(ctx, "{"+choice+"}", operation, scope); err != nil {
		t.Fatalf("alternate model did not really activate: %v", err)
	}

	verifyModelIngressRemainsReadableAndCancels(t, ctx, m, ready, source)

	const prefix = "/Xenova/bge-m3/resolve/4de13258303883538bd53b696b452bf8099f0858/"
	paths := map[string]string{
		prefix + "onnx/model_quantized.onnx": "model_quantized.onnx",
		prefix + "tokenizer.json":            "tokenizer.json",
	}
	mirror := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, req *http.Request) {
		name, ok := paths[req.URL.Path]
		if !ok {
			http.NotFound(w, req)
			return
		}
		file, err := os.Open(filepath.Join(source, name))
		if err != nil {
			http.Error(w, "fixture is not available", http.StatusInternalServerError)
			return
		}
		defer file.Close()
		http.ServeContent(w, req, name, time.Time{}, file)
	}))
	defer mirror.Close()
	before := m.modelHTTPClient
	m.modelHTTPClient = mirror.Client()
	defer func() { m.modelHTTPClient = before }()
	if err := m.DownloadModel(ctx, "m3", "quantized", mirror.URL); err != nil {
		t.Fatalf("ordinary quantized M3 download through real publisher: %v", err)
	}
	weightPath, tokenizerPath, available := m.managedM3Pair(ready)
	if !available {
		t.Fatal("M3 never appeared as a complete managed pair")
	}
	for path, name := range map[string]string{weightPath: "quantized", tokenizerPath: "tokenizer"} {
		if sum, err := fileHash(path); err != nil || sum != modelFileSpecs[name].sha256 {
			t.Fatalf("managed M3 pin did not match: %v", err)
		}
	}
	if _, err := m.uninstallM3(ctx); err != nil {
		t.Fatalf("desktop uninstall failed to remove inactive M3: %v", err)
	}
	if _, _, stillPresent := m.managedM3Pair(ready); stillPresent {
		t.Fatal("desktop uninstall left the managed M3 pair installed")
	}
	after, _, err := m.Health(ctx)
	if err != nil || !after.StoreReady || after.PID != ready.PID || after.InstanceID != ready.InstanceID ||
		after.Generation != ready.Generation || after.AccountID != ready.AccountID {
		t.Fatalf("desktop M3 flow interrupted the serving Runtime: %v", err)
	}
}
