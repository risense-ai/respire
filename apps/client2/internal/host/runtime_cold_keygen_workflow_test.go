//go:build linux || windows || (darwin && cgo)

package host

import (
	"bytes"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"testing"
)

func TestColdKeygenWorkflowVerifiesSlotAndReceipt(t *testing.T) {
	m, binding, _ := newColdKeygenCustodyFixture(t)
	material, err := newRuntimeVaultMaterial(binding.Operation)
	if err != nil {
		t.Fatal(err)
	}
	custody, err := m.prepareColdKeygenCustody(binding, material)
	if err != nil {
		t.Fatal(err)
	}
	const desktop = "desktop-" + coldTestOperation
	factor := "not the original synthetic factor"
	store := hostCredentialStore{
		backends: []string{material.Backend},
		read: func(backend, service, account string) (*string, error) {
			if backend != material.Backend || service != "rsrs" || account != "super:"+material.Account {
				t.Fatal("looked up unrelated credential")
			}
			return &factor, nil
		},
	}
	exchangeCalled := false
	exchange := func(original runtimeRegisterMaterial) (string, error) {
		exchangeCalled = true
		if original != material {
			t.Fatal("did not send the original validated material")
		}
		return "", errors.New("synthetic unavailable child")
	}
	if _, revealed, err := m.finishNativeColdKeygen(desktop, custody, false, store, exchange); err == nil || exchangeCalled || revealed != "" {
		t.Fatal("changed factor reached the transport or was revealed")
	}
	factor = material.Super
	if _, revealed, err := m.finishNativeColdKeygen(desktop, custody, false, store, exchange); err == nil || !exchangeCalled || revealed != "" {
		t.Fatal("unconfirmed transport result exposed a factor")
	}
	raw, err := json.Marshal(map[string]any{
		"vault_version": 4, "auth_operation_id": binding.Operation, "kdf_salt": material.Salt, "wrapped_urk": material.Wrapped,
		"urk_nonce": material.Nonce, "keyring_account": material.Account, "keyring_backend": material.Backend,
	})
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(binding.Profile, "session.json"), raw, 0600); err != nil {
		t.Fatal(err)
	}
	commit := func(runtimeRegisterMaterial) (string, error) { return controlDigest(raw), nil }
	for _, query := range []bool{false, true} {
		reply, oneTime, err := m.finishNativeColdKeygen(desktop, custody, query, store, commit)
		if err != nil || bytes.Contains(reply, []byte(material.Super)) || (query && oneTime != "") || (!query && oneTime != material.Super) {
			t.Fatalf("unexpected completion: query=%t err=%v", query, err)
		}
	}
	if err := os.WriteFile(filepath.Join(binding.Profile, "session.json"), append(raw, '\n'), 0600); err != nil {
		t.Fatal(err)
	}
	if _, revealed, err := m.finishNativeColdKeygen(desktop, custody, true, store, commit); err == nil || revealed != "" {
		t.Fatal("accepted changed session bytes")
	}
}
