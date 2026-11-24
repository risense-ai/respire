//go:build linux || windows || (darwin && cgo)

package host

import (
	"bytes"
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/hkdf"
	"crypto/rand"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"testing"
)

func disposableV4Session(t *testing.T, profile string) (string, string) {
	t.Helper()
	entropy := make([]byte, 18)
	salt := make([]byte, 16)
	root := make([]byte, 32)
	rand.Read(entropy)
	rand.Read(salt)
	rand.Read(root)
	defer clear(root)
	key, err := hkdf.Key(sha256.New, entropy, salt, "onememory:kek:v4", 32)
	if err != nil {
		t.Fatal(err)
	}
	defer clear(key)
	block, err := aes.NewCipher(key)
	if err != nil {
		t.Fatal(err)
	}
	aead, err := cipher.NewGCM(block)
	if err != nil {
		t.Fatal(err)
	}
	nonce := make([]byte, aead.NonceSize())
	rand.Read(nonce)
	session, err := json.Marshal(map[string]any{
		"user": "client2-disposable-runtime", "addr": "https://respire.invalid",
		"vault_version": 4, "kdf_salt": hex.EncodeToString(salt),
		"urk_nonce": hex.EncodeToString(nonce), "wrapped_urk": hex.EncodeToString(aead.Seal(nil, nonce, root, nil)),
	})
	if err != nil {
		t.Fatal(err)
	}
	path := filepath.Join(profile, "session.json")
	file, err := os.OpenFile(path, os.O_WRONLY|os.O_CREATE|os.O_EXCL, 0600)
	if err != nil {
		t.Fatal(err)
	}
	_, writeErr := file.Write(session)
	closeErr := file.Close()
	if writeErr != nil || closeErr != nil {
		t.Fatalf("write isolated Vault metadata: %v, %v", writeErr, closeErr)
	}
	return "A3-" + hex.EncodeToString(entropy), controlDigest(session)
}

func realUnlockV2(t *testing.T, ctx context.Context, m *Manager, health HealthInfo, profile, digest, token, factor string, want int) {
	t.Helper()
	frame, err := json.Marshal(map[string]any{
		"schema_version": 2, "profile": profile, "session_sha256": digest,
		"instance_id": health.InstanceID, "generation": health.Generation,
		"super_password": factor, "login_password": nil, "legacy_secret": nil,
		"provider_update": map[string]any{"kind": "replace", "credentials": []any{}},
	})
	if err != nil {
		t.Fatal(err)
	}
	defer clear(frame)
	request, err := http.NewRequestWithContext(ctx, http.MethodPost, m.Endpoint+"/api/runtime/unlock", bytes.NewReader(frame))
	if err != nil {
		t.Fatal(err)
	}
	request.Header.Set("Authorization", "Bearer "+token)
	request.Header.Set("Content-Type", "application/json")
	res, err := m.localClient.Do(request)
	if err != nil {
		t.Fatal(err)
	}
	defer res.Body.Close()
	if res.StatusCode != want {
		t.Fatalf("Runtime unlock HTTP %d, want %d", res.StatusCode, want)
	}
	if want == http.StatusOK {
		var receipt struct {
			Schema   int  `json:"schema_version"`
			Ready    bool `json:"store_ready"`
			Provider struct {
				State string `json:"state"`
			} `json:"provider_status"`
		}
		if err := json.NewDecoder(io.LimitReader(res.Body, 65536)).Decode(&receipt); err != nil ||
			receipt.Schema != 2 || !receipt.Ready || receipt.Provider.State != "ready" {
			t.Fatalf("Runtime did not confirm a ready store and empty provider snapshot: %v", err)
		}
	}
}

func TestRealRuntimeUnlockV2(t *testing.T) {
	m, info := realRuntimeManager(t)
	ctx := realRuntimeContext(t)
	t.Cleanup(func() { cleanupRealRuntime(t, m, info) })
	secret, digest := disposableV4Session(t, m.DataRoot)
	// Use production installed-image startup, but supply the entirely synthetic
	// factors directly over the real host protocol. This never asks Client2 to
	// read the workstation's native credential vault.
	locked, err := m.startDetached(ctx, info)
	if err != nil {
		t.Fatal(err)
	}
	_, profile, account, err := m.startupScope()
	if err != nil {
		t.Fatal(err)
	}
	if err := m.verifyScopedRuntimeOwner(ctx, locked, info, profile, account); err != nil {
		t.Fatal(err)
	}
	if locked.StoreReady || locked.CredentialState != "required" {
		t.Fatal("synthetic Runtime must start locked")
	}
	token, err := m.runtimeUnlockToken()
	if err != nil {
		t.Fatal(err)
	}
	wrong := "A3-00" + secret[5:]
	if wrong == secret {
		wrong = "A3-01" + secret[5:]
	}
	realUnlockV2(t, ctx, m, locked, profile, digest, token, wrong, http.StatusConflict)
	retained, _, err := m.Health(ctx)
	if err != nil || retained != locked {
		t.Fatalf("failed unlock changed the serving identity or readiness: %v", err)
	}
	realUnlockV2(t, ctx, m, locked, profile, digest, token, secret, http.StatusOK)
	ready, _, err := m.Health(ctx)
	if err != nil || !ready.StoreReady || ready.CredentialState != "available" ||
		ready.InstanceID != locked.InstanceID || ready.AccountID != locked.AccountID || ready.PID != locked.PID {
		t.Fatalf("the same native process did not report an unlocked library: %v", err)
	}
}

func TestRealRuntimeNativeUnlockBridge(t *testing.T) {
	m, info := realRuntimeManager(t)
	ctx := realRuntimeContext(t)
	t.Cleanup(func() { cleanupRealRuntime(t, m, info) })
	factor, _ := disposableV4Session(t, m.DataRoot)
	locked, err := m.startDetached(ctx, info)
	if err != nil {
		t.Fatal(err)
	}
	if locked.StoreReady {
		t.Fatal("isolated Runtime unexpectedly started unlocked")
	}
	reads := 0
	provided := "A3-00" + factor[5:]
	if provided == factor {
		provided = "A3-01" + factor[5:]
	}
	store := hostCredentialStore{
		backends: []string{"disposable"},
		read: func(backend, service, account string) (*string, error) {
			if backend != "disposable" {
				t.Fatal("test attempted to leave its disposable credential store")
			}
			if service == "rsrs" && account == "super:client2-disposable-runtime" {
				reads++
				return &provided, nil
			}
			return nil, nil
		},
	}
	mismatched := locked
	mismatched.AccountID = controlDigest([]byte("different-disposable-account"))
	if err := m.unlockNativeRuntime(ctx, mismatched, store); err == nil || reads != 0 {
		t.Fatal("mismatched serving account reached its credential store")
	}
	if err := m.unlockNativeRuntime(ctx, locked, store); err == nil || reads != 1 {
		t.Fatal("wrong synthetic factor did not fail closed")
	}
	stillLocked, _, err := m.Health(ctx)
	if err != nil || stillLocked != locked {
		t.Fatalf("failed host unlock changed the serving identity or readiness: %v", err)
	}
	provided = factor
	if err := m.unlockNativeRuntime(ctx, locked, store); err != nil {
		t.Fatal(err)
	}
	ready, _, err := m.Health(ctx)
	if err != nil || reads != 2 || !ready.StoreReady || ready.InstanceID != locked.InstanceID {
		t.Fatalf("production Manager unlock did not open the same Runtime, reads=%d, error=%v", reads, err)
	}
	next, err := m.acquireStartup(ctx, info)
	if err != nil {
		t.Fatalf("unlock bridge retained the installation exclusion: %v", err)
	}
	t.Cleanup(func() { next.Close() })
	if _, err := next.Finish(nil); err != nil {
		t.Fatal(err)
	}
	requireStartupReaped(t, next)
}
