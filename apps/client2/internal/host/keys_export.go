//go:build linux || windows || (darwin && cgo)

package host

import "respire-client2/internal/i18n"

import (
	"context"
	"crypto/aes"
	"crypto/cipher"
	"crypto/hkdf"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

type exportVault struct {
	runtimeSession
	Server  string `json:"addr"`
	Salt    string `json:"kdf_salt"`
	Wrapped string `json:"wrapped_urk"`
	Nonce   string `json:"urk_nonce"`
}

func (m *Manager) desktopKeysExport(ctx context.Context, args []string) (reply json.RawMessage, err error) {
	if len(args) != 1 {
		return nil, errors.New(i18n.Message850defce43741f72)
	}
	out, ok := strings.CutPrefix(args[0], "--out=")
	if !ok || out == "" {
		return nil, errors.New(i18n.Message850defce43741f72)
	}
	takeover, err := m.lockRuntimeTakeover(ctx)
	if err != nil {
		return nil, err
	}
	defer func() { err = errors.Join(err, takeover.Close()) }()
	root, err := canonicalSessionDirectory(m.DataRoot)
	if err != nil {
		return nil, err
	}
	profile, err := selectedRuntimeLoginProfile(root)
	if err != nil {
		return nil, err
	}
	raw, err := readCredentialConfig(filepath.Join(profile, "session.json"), controlFrameLimit)
	if err != nil {
		return nil, err
	}
	defer clear(raw)
	var vault exportVault
	if json.Unmarshal(raw, &vault) != nil || vault.Salt == "" || vault.Wrapped == "" || vault.Nonce == "" {
		return nil, errors.New(i18n.Messaged6870ddd3b6903c0)
	}
	version := uint64(1)
	if vault.Version != nil {
		version = *vault.Version
	}
	if version < 1 {
		return nil, errors.New(i18n.Message0b7d6e56c11ca07d)
	}
	factors, err := vault.exportFactors(nativeRuntimeCredentialStore(ctx), version)
	if err != nil {
		return nil, err
	}
	body := fmt.Sprintf("Respire key recovery notes (keep this file private)\n\nuser: %s\nserver: %s\n\n%s\nvault_version: %d\nkdf_salt: %s\nwrapped_urk: %s\nurk_nonce: %s\n",
		vault.User, vault.Server, factors, version, vault.Salt, vault.Wrapped, vault.Nonce)
	if err := writePrivateKeysExport(out, body); err != nil {
		return nil, err
	}
	return json.Marshal(map[string]any{"command": "keys-export", "status": "ok", "summary": map[string]any{
		"path": out, "version": version, "user": vault.User,
	}, "actions": []string{"protect file; it contains decrypt secrets"}})
}

func (vault exportVault) exportFactors(store hostCredentialStore, version uint64) (string, error) {
	account, err := vault.credentialAccount()
	if err != nil {
		return "", err
	}
	read := func(inline *string, backend, name string) (string, error) {
		value := inline
		if value == nil || *value == "" {
			stored, err := store.readSlot(backend, "rsrs", name+":"+account, false)
			if err != nil {
				return "", err
			}
			value = stored
		}
		if value == nil || *value == "" {
			return "", fmt.Errorf(i18n.Message23d601697f6e2cb4, name)
		}
		return *value, nil
	}
	if version == 1 {
		secret, err := read(vault.LegacySecret, vault.LegacyBackend, "legacy-secret")
		if err != nil {
			return "", err
		}
		return "original login password: required, not included in this export\nAccount Secret: " + secret + "\n", nil
	}
	if version >= 4 {
		super, _, _, err := vault.hostFactors(store)
		if err != nil {
			return "", err
		}
		if err := vault.verifyExportSuper(*super); err != nil {
			return "", err
		}
		return "super password: " + *super + "\n", nil
	}
	super, err := read(vault.LegacySuper, vault.Backend, "super")
	if err != nil {
		return "", err
	}
	text := "super password (passphrase): " + super + "\n"
	if version == 3 {
		secret, err := read(vault.SecretKey, vault.LegacyBackend, "legacy-secret")
		if err != nil {
			return "", err
		}
		text += "Secret Key (recovery code): " + secret + "\n"
	}
	return text, nil
}

func (vault exportVault) verifyExportSuper(super string) error {
	urk, err := vault.unwrapSuperKey(super)
	clear(urk)
	return err
}

func (vault exportVault) unwrapSuperKey(super string) ([]byte, error) {
	encoded := strings.Map(func(r rune) rune {
		if r >= '0' && r <= '9' || r >= 'a' && r <= 'f' || r >= 'A' && r <= 'F' {
			return r
		}
		return -1
	}, strings.TrimPrefix(strings.TrimSpace(super), "A3-"))
	entropy, err := hex.DecodeString(encoded)
	if err != nil || len(entropy) != 18 {
		return nil, errors.New(i18n.Message5bb3df337c199492)
	}
	defer clear(entropy)
	salt, saltErr := hex.DecodeString(vault.Salt)
	if saltErr != nil {
		return nil, errors.New(i18n.Messagef050a3f9e6fa5cb8)
	}
	domain := "onememory:kek:v4"
	if strings.HasPrefix(vault.Wrapped, "rsrs:v1:") {
		domain = "rsrs:kek:v4"
	}
	key, err := hkdf.Key(sha256.New, entropy, salt, domain, 32)
	if err != nil {
		return nil, err
	}
	defer clear(key)
	return vault.unwrapVaultKey(key)
}

func (vault exportVault) unwrapVaultKey(key []byte) ([]byte, error) {
	nonce, nonceErr := hex.DecodeString(vault.Nonce)
	ciphertext, wrapErr := hex.DecodeString(strings.TrimPrefix(vault.Wrapped, "rsrs:v1:"))
	if nonceErr != nil || wrapErr != nil || len(nonce) != 12 {
		return nil, errors.New(i18n.Messagef050a3f9e6fa5cb8)
	}
	block, err := aes.NewCipher(key)
	if err != nil {
		return nil, err
	}
	aead, err := cipher.NewGCM(block)
	if err != nil {
		return nil, err
	}
	urk, err := aead.Open(nil, nonce, ciphertext, nil)
	if err != nil || len(urk) != 32 {
		clear(urk)
		return nil, errors.New(i18n.Message48a49103df72f159)
	}
	return urk, nil
}

func writePrivateKeysExport(out, body string) (err error) {
	stage, err := os.CreateTemp(filepath.Dir(out), ".keys-export-*")
	if err != nil {
		return err
	}
	path := stage.Name()
	defer func() {
		if err != nil {
			removeErr := os.Remove(path)
			if !errors.Is(removeErr, os.ErrNotExist) {
				err = errors.Join(err, removeErr)
			}
		}
	}()
	_, writeErr := stage.WriteString(body)
	if err := errors.Join(writeErr, stage.Sync(), stage.Close()); err != nil {
		return err
	}
	return publishSessionFile(path, out)
}
