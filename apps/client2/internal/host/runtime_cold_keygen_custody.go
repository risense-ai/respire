//go:build linux || windows || (darwin && cgo)

package host

import (
	"bytes"
	"encoding/hex"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
)

const coldKeygenCustodyLimit = 16 << 10

var errColdKeygenCustody = errors.New("Client2 cold keygen custody checkpoint is invalid or changed")

// Derived from the verified selected Runtime and original desktop operation,
// not from a custody file. The same binding is required on recovery.
type coldKeygenCustodyBinding struct {
	Operation string `json:"operation_id"`
	Profile   string `json:"profile"`
	Image     string `json:"image"`
	ImageSHA  string `json:"image_sha256"`
}

// Deliberately not runtimeRegisterMaterial: plaintext cannot be marshaled here.
type coldKeygenWrappedMaterial struct {
	Account string `json:"keyring_account"`
	Backend string `json:"keyring_backend"`
	Salt    string `json:"kdf_salt"`
	Wrapped string `json:"wrapped_urk"`
	Nonce   string `json:"urk_nonce"`
}

type coldKeygenIntent struct {
	Schema   int                       `json:"schema"`
	Binding  coldKeygenCustodyBinding  `json:"binding"`
	Material coldKeygenWrappedMaterial `json:"material"`
}

type coldKeygenIntentFile struct {
	Intent coldKeygenIntent `json:"intent"`
	SHA    string           `json:"sha256"`
}

type coldKeygenSendFile struct {
	Schema    int    `json:"schema"`
	Operation string `json:"operation_id"`
	IntentSHA string `json:"intent_sha256"`
}

type coldKeygenCustody struct {
	Intent coldKeygenIntent
	Digest string
	Sent   bool
}

func (m *Manager) prepareColdKeygenCustody(binding coldKeygenCustodyBinding, material runtimeRegisterMaterial) (coldKeygenCustody, error) {
	intent := coldKeygenIntent{
		Schema: 1, Binding: binding,
		Material: coldKeygenWrappedMaterial{Account: material.Account, Backend: material.Backend,
			Salt: material.Salt, Wrapped: material.Wrapped, Nonce: material.Nonce},
	}
	path, _, err := m.coldKeygenCustodyPaths(binding, true)
	if err != nil {
		return coldKeygenCustody{}, err
	}
	record, data, err := encodeColdKeygenIntent(intent)
	if err != nil {
		return coldKeygenCustody{}, err
	}
	if err := writeRuntimeAccountRecord(path, data, nil); err != nil {
		return coldKeygenCustody{}, err
	}
	return coldKeygenCustody{Intent: intent, Digest: record.SHA}, nil
}

func (m *Manager) loadColdKeygenCustody(binding coldKeygenCustodyBinding) (coldKeygenCustody, error) {
	path, sent, err := m.coldKeygenCustodyPaths(binding, false)
	if err != nil {
		return coldKeygenCustody{}, err
	}
	raw, err := readCredentialConfig(path, coldKeygenCustodyLimit)
	if err != nil {
		return coldKeygenCustody{}, err
	}
	var saved coldKeygenIntentFile
	if json.Unmarshal(raw, &saved) != nil || saved.Intent.Binding != binding {
		return coldKeygenCustody{}, errColdKeygenCustody
	}
	_, expected, err := encodeColdKeygenIntent(saved.Intent)
	if err != nil || !bytes.Equal(raw, expected) {
		return coldKeygenCustody{}, errColdKeygenCustody
	}
	out := coldKeygenCustody{Intent: saved.Intent, Digest: saved.SHA}
	raw, err = readCredentialConfig(sent, coldKeygenCustodyLimit)
	if errors.Is(err, os.ErrNotExist) {
		return out, nil
	}
	if err != nil {
		return coldKeygenCustody{}, err
	}
	expected, err = encodeColdKeygenSend(out)
	if err != nil || !bytes.Equal(raw, expected) {
		return coldKeygenCustody{}, errColdKeygenCustody
	}
	out.Sent = true
	return out, nil
}

// Only a successful fresh checkpoint authorizes a first dispatch. An I/O
// error is ambiguous: leave the record and load/query, never retry a send.
func (m *Manager) checkpointColdKeygenSend(prepared coldKeygenCustody) error {
	binding := prepared.Intent.Binding
	current, err := m.loadColdKeygenCustody(binding)
	if err != nil {
		return err
	}
	if current != prepared || current.Sent {
		return errColdKeygenCustody
	}
	_, path, err := m.coldKeygenCustodyPaths(binding, false)
	if err != nil {
		return err
	}
	data, err := encodeColdKeygenSend(current)
	if err != nil {
		return err
	}
	return writeRuntimeAccountRecord(path, data, nil)
}

func (m *Manager) coldKeygenCustodyPaths(binding coldKeygenCustodyBinding, create bool) (string, string, error) {
	if !hostOperationID.MatchString(binding.Operation) || !coldKeygenHex(binding.ImageSHA, 32) ||
		!coldKeygenAbsolutePath(binding.Image) || !coldKeygenAbsolutePath(binding.Profile) ||
		!coldKeygenAbsolutePath(m.Root) || !samePath(m.Root, filepath.Join(m.DataRoot, "client2")) {
		return "", "", errColdKeygenCustody
	}
	if err := checkRuntimeAccountDirectory(m.Root); err != nil {
		return "", "", err
	}
	dir := filepath.Join(m.Root, "cold-keygen")
	if create {
		if err := os.Mkdir(dir, 0700); err != nil && !errors.Is(err, os.ErrExist) {
			return "", "", err
		}
	}
	if err := checkRuntimeAccountDirectory(dir); err != nil {
		return "", "", err
	}
	return filepath.Join(dir, binding.Operation+".json"), filepath.Join(dir, binding.Operation+".sent.json"), nil
}

func encodeColdKeygenIntent(intent coldKeygenIntent) (coldKeygenIntentFile, []byte, error) {
	material := intent.Material
	wrapped, hasPrefix := strings.CutPrefix(material.Wrapped, "rsrs:v1:")
	if intent.Schema != 1 || material.Account != offlineKeygenAlias(intent.Binding.Operation) ||
		material.Backend != runtimeLoginBackend() || !coldKeygenHex(material.Salt, 16) ||
		!coldKeygenHex(material.Nonce, 12) || !hasPrefix || !coldKeygenHex(wrapped, 48) {
		return coldKeygenIntentFile{}, nil, errColdKeygenCustody
	}
	body, err := json.Marshal(intent)
	if err != nil || len(body) > coldKeygenCustodyLimit-128 {
		return coldKeygenIntentFile{}, nil, errColdKeygenCustody
	}
	out := coldKeygenIntentFile{Intent: intent, SHA: controlDigest(body)}
	data, err := json.Marshal(out)
	return out, data, err
}

func encodeColdKeygenSend(custody coldKeygenCustody) ([]byte, error) {
	return json.Marshal(coldKeygenSendFile{Schema: 1, Operation: custody.Intent.Binding.Operation, IntentSHA: custody.Digest})
}

func coldKeygenHex(value string, size int) bool {
	if len(value) != 2*size || strings.ToLower(value) != value {
		return false
	}
	_, err := hex.DecodeString(value)
	return err == nil
}

func coldKeygenAbsolutePath(value string) bool {
	return len(value) <= 4096 && !strings.ContainsRune(value, 0) && filepath.IsAbs(value) && filepath.Clean(value) == value
}
