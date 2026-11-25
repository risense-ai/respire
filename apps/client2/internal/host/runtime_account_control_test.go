package host

import (
	"bytes"
	"context"
	"encoding/json"
	"io"
	"net"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestRuntimeLoginOwnerProcess(t *testing.T) {
	if os.Getenv("RESPIRE_TEST_LOGIN_OWNER") == "1" {
		if _, err := io.Copy(io.Discard, os.Stdin); err != nil {
			t.Fatal(err)
		}
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	exe, err := os.Executable()
	if err != nil {
		t.Fatal(err)
	}
	child := exec.CommandContext(ctx, exe, "-test.run=^TestRuntimeLoginOwnerProcess$")
	child.Env = append(os.Environ(), "RESPIRE_TEST_LOGIN_OWNER=1")
	configureCommand(child)
	input, err := child.StdinPipe()
	if err != nil {
		t.Fatal(err)
	}
	if err := child.Start(); err != nil {
		t.Fatal(err)
	}
	waited := false
	defer func() {
		if !waited {
			_ = child.Process.Kill()
			_ = child.Wait()
		}
	}()
	birth, err := runtimeControlProcessBirth(ctx, child.Process.Pid)
	if err != nil || birth == "" {
		t.Fatalf("live maintenance owner identity is missing: %v", err)
	}
	root, err := canonicalSessionDirectory(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	const operation = "00000000-0000-4000-8000-000000000001"
	record := runtimeAccountRecord{Root: root, Source: root, Token: operation}
	record.Original.Identity = runtimeAuthIdentity{Kind: "absent"}
	owner := runtimeMaintenanceMarker{Schema: 1, Phase: "Paused", Token: operation, ExpectedProfile: root,
		OwnerPID: child.Process.Pid, OwnerBirth: birth}
	original, err := json.Marshal(owner)
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(root, "maintenance.json")
	if err := os.WriteFile(path, original, 0600); err != nil {
		t.Fatal(err)
	}
	var manager Manager
	if _, err := manager.claimRuntimeLogin(ctx, record, original, path, root); err == nil {
		t.Fatal("live Client2 maintenance ownership was replaced")
	}
	if err := input.Close(); err != nil {
		t.Fatal(err)
	}
	if err := child.Wait(); err != nil {
		t.Fatal(err)
	}
	waited = true
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatal(err)
	}
	manager.Endpoint = "http://" + listener.Addr().String()
	if err := listener.Close(); err != nil {
		t.Fatal(err)
	}
	recovered, err := manager.claimRuntimeLogin(ctx, record, original, path, root)
	if err != nil {
		t.Fatal(err)
	}
	if json.Unmarshal(recovered, &owner) != nil || owner.OwnerPID != os.Getpid() ||
		owner.Token != record.Token || owner.Phase != "Paused" {
		t.Fatal("recovery did not retain the original paused operation")
	}
}

func TestDesktopPasswordLoginUsesRuntimeInstallation(t *testing.T) {
	root, err := filepath.EvalSymlinks(t.TempDir())
	if err != nil {
		t.Fatal(err)
	}
	t.Setenv("RSRS_DATA_DIR", root)
	manager, err := New()
	if err != nil {
		t.Fatal(err)
	}
	manager.hostAuthority = &hostAuthorityFixture{}
	manager.ClientVersion = "2.0.0-dev.20"
	id := "desktop-00000000-0000-4000-8000-000000000001"
	if _, err := manager.Submit("auth_login", id,
		json.RawMessage(`{"user":"isolated","addr":"https://respire.example.invalid"}`),
		true, `{"password":"fixture only"}`); err != nil {
		t.Fatal(err)
	}
	ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
	defer cancel()
	for {
		job, err := manager.Operation(id)
		if err != nil {
			t.Fatal(err)
		}
		if job.State != "running" {
			if job.State != "failed" || !strings.Contains(job.Error, "runtime.json") {
				t.Fatalf("desktop login did not use the native Runtime installation: %s", job.Error)
			}
			return
		}
		select {
		case <-ctx.Done():
			t.Fatal("desktop password login did not report the missing Runtime installation")
		case <-time.After(20 * time.Millisecond):
		}
	}
}

func TestRuntimeAuthSessionIdentity(t *testing.T) {
	const operation = "00000000-0000-4000-8000-000000000001"
	const factors = `"kdf_salt":"salt","wrapped_urk":"wrap","urk_nonce":"nonce","keyring_account":"login-test","keyring_backend":"native","auth_operation_id":"` + operation + `"`
	for _, tc := range []struct {
		name, session, kind string
		invalid             bool
	}{
		{name: "missing", kind: "absent"},
		{name: "named account", session: `{"user":"original user"}`, kind: "bound"},
		{name: "keygen", session: `{"vault_version":4,` + factors + `}`, kind: "unbound"},
		{name: "offline v1", session: `{"vault_version":1,"user":"","addr":"","legacy_secret_backend":"native",` + factors + `}`, kind: "unbound"},
		{name: "offline v4", session: `{"vault_version":4,"user":"","addr":"",` + factors + `}`, kind: "unbound"},
		{name: "unexpected field", session: `{"vault_version":4,"token":"extra",` + factors + `}`, invalid: true},
		{name: "no user or keygen", session: `{}`, invalid: true},
		{name: "null is not missing", session: `null`, invalid: true},
		{name: "null user is not absent", session: `{"user":null}`, invalid: true},
		{name: "invalid UUID version", session: `{"vault_version":4,"kdf_salt":"salt","wrapped_urk":"wrap","urk_nonce":"nonce","keyring_account":"login-test","keyring_backend":"native","auth_operation_id":"00000000-0000-1000-8000-000000000001"}`, invalid: true},
	} {
		t.Run(tc.name, func(t *testing.T) {
			var original []byte
			if tc.name != "missing" {
				original = []byte(tc.session)
			}
			identity, err := runtimeAuthIdentityOf(original)
			if tc.invalid {
				if err == nil {
					t.Fatal("invalid session admitted an account identity")
				}
				return
			}
			if err != nil || identity.Kind != tc.kind {
				t.Fatalf("wrong account identity: %v", err)
			}
			if original != nil && identity.SessionDigest != controlDigest(original) {
				t.Fatal("account identity was not bound to the exact session bytes")
			}
			if tc.kind == "bound" && (identity.AccountID == nil || *identity.AccountID != controlDigest([]byte("original user"))) {
				t.Fatal("named account hash differs")
			}
			if tc.kind == "unbound" && (identity.OperationID != operation || identity.AccountID != nil) {
				t.Fatal("unbound session lost its original operation")
			}
		})
	}
}

func TestRuntimeLoginReceiptBindsPrivateChallenge(t *testing.T) {
	record := runtimeAccountRecord{
		OperationID: "00000000-0000-4000-8000-000000000001",
		Source:      t.TempDir(),
		User:        "fixture",
		Server:      "https://respire.example.invalid/",
	}
	const challenge = "private challenge fixture"
	digest := controlDigest([]byte(challenge))
	decode := func(operation, digest string, inputs []string) (runtimeLoginResult, error) {
		t.Helper()
		summary := map[string]any{
			"receipt": map[string]any{
				"operation_id": operation, "outcome": "awaiting_input", "source_profile": record.Source,
				"login": map[string]any{"user": record.User, "addr": strings.TrimSuffix(record.Server, "/"), "required_inputs": inputs},
			},
			"handoff": map[string]any{"kind": "challenge", "payload": challenge, "sha256": digest},
		}
		raw, err := json.Marshal(map[string]any{"command": "auth-account", "status": "pending", "summary": summary})
		if err != nil {
			t.Fatal(err)
		}
		return readRuntimeLoginResult(rpcResponse{Envelope: raw}, record)
	}
	result, err := decode(record.OperationID, digest, []string{"totp"})
	if err != nil || result.Handoff == nil || result.Handoff.Challenge == nil || *result.Handoff.Challenge != challenge {
		t.Fatalf("valid challenge did not reach the private coordinator: %v", err)
	}
	public, err := json.Marshal(result.Receipt)
	if err != nil || bytes.Contains(public, []byte(challenge)) {
		t.Fatalf("account receipt contains the private challenge: %v", err)
	}
	for _, test := range []struct {
		operation, digest string
		inputs            []string
	}{
		{"00000000-0000-4000-8000-000000000002", digest, []string{"totp"}},
		{record.OperationID, strings.Repeat("0", 64), []string{"totp"}},
		{record.OperationID, digest, []string{"totp", "totp"}},
		{record.OperationID, digest, []string{"host_credentials"}},
	} {
		if _, err := decode(test.operation, test.digest, test.inputs); err == nil {
			t.Fatal("invalid challenge accepted")
		}
	}
}
