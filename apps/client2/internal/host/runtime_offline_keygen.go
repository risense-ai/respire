//go:build linux || windows || (darwin && cgo)

package host

import "respire-client2/internal/i18n"

import (
	"context"
	"crypto/subtle"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
	"time"
)

const offlineKeygenFile = "offline-keygen.json"

type runtimeOfflineKeygenRecord struct {
	Schema       int                 `json:"schema"`
	Operation    string              `json:"operation_id"`
	Force        bool                `json:"force"`
	Root         string              `json:"root"`
	Source       string              `json:"source"`
	Profile      string              `json:"profile"`
	Image        string              `json:"binary"`
	ImageDigest  string              `json:"image_sha256"`
	Token        string              `json:"token"`
	Coordinator  string              `json:"coordinator"`
	MarkerDigest string              `json:"marker_sha256"`
	NextMarker   string              `json:"pending_marker_sha256,omitempty"`
	Original     runtimeAuthIdentity `json:"original_identity"`
	Stage        string              `json:"stage"`
	Session      *string             `json:"session_sha256"`
	Material     json.RawMessage     `json:"material"`
	WasServing   bool                `json:"was_serving"`
	StoreReady   bool                `json:"store_ready"`
}

func (m *Manager) recoverLegacyOfflineKeygen(ctx context.Context, id string) (raw json.RawMessage, recovery string, err error) {
	ctx, cancel := context.WithTimeout(ctx, 90*time.Second)
	defer cancel()
	op := strings.TrimPrefix(id, "desktop-")
	if !hostOperationID.MatchString(op) || m.ClientVersion == "" {
		return nil, "", errors.New(i18n.Messagea29f712bcbeb6a56)
	}
	defer func() {
		if err != nil {
			err = &unconfirmedError{id: id, cause: err}
		}
	}()
	takeover, err := m.lockRuntimeTakeover(ctx)
	if err != nil {
		return nil, "", err
	}
	defer takeover.Close()
	root, err := canonicalSessionDirectory(m.DataRoot)
	if err != nil {
		return nil, "", err
	}
	profile, err := selectedRuntimeLoginProfile(root)
	if err != nil {
		return nil, "", err
	}
	directory := filepath.Join(root, "runtime", "autostart")
	markerPath := filepath.Join(directory, "maintenance.json")
	activePath := filepath.Join(directory, offlineKeygenFile)
	receiptPath := filepath.Join(directory, "offline-keygen-result-"+op+".json")
	markerArchive := filepath.Join(directory, "maintenance.keygen-"+op+".json")
	if err := checkRuntimeAccountDirectory(directory); err != nil {
		return nil, "", err
	}

	info, err := m.Installed()
	if err != nil {
		return nil, "", err
	}
	if err := m.verifyOfflineKeygenRuntime(info); err != nil {
		return nil, "", err
	}
	image, err := canonicalSessionDirectory(filepath.Join(m.runtimeDir(info.Version), runtimeBinary()))
	if err != nil {
		return nil, "", err
	}
	record := runtimeOfflineKeygenRecord{
		Schema: 1, Operation: op, Root: root, Source: profile, Profile: profile, Image: image,
		ImageDigest: info.SHA256, Original: runtimeAuthIdentity{Kind: "absent"},
	}
	store := nativeRuntimeCredentialStore(ctx)
	data, err := readCredentialConfig(receiptPath, runtimeMaintenanceLimit)
	if err == nil {
		committed, err := record.readOriginal(data)
		if err != nil || committed.Stage != "committed" {
			return nil, "", errors.New(i18n.Messageaf356698ad4061b8)
		}
		if err := committed.verifySession(); err != nil {
			return nil, "", err
		}
		if err := committed.verifyRetiredMarker(markerArchive); err != nil {
			return nil, "", err
		}
		return offlineKeygenEnvelope(committed)
	}
	if !errors.Is(err, os.ErrNotExist) {
		return nil, "", err
	}
	return m.queryOfflineKeygen(ctx, record, activePath, markerPath, markerArchive, receiptPath, store)
}

func (m *Manager) verifyOfflineKeygenRuntime(info InstalledRuntime) error {
	raw, err := os.ReadFile(m.runtimeReceiptPath(info.Version))
	if err != nil {
		return err
	}
	var release sharedRuntimeReceipt
	var metadata struct {
		Capabilities map[string]string `json:"capabilities"`
	}
	if json.Unmarshal(raw, &release) != nil || release.Release == nil || release.Version != info.Version ||
		release.SHA256 != info.SHA256 ||
		json.Unmarshal([]byte(release.Release.JSON), &metadata) != nil || metadata.Capabilities["offline_keygen_handoff"] != "1" {
		return errors.New(i18n.Message44b671b0be1bb5ee)
	}
	return verifyReleaseTree(m.runtimeDir(info.Version), release)
}

func requireUninitializedKeygenProfile(profile string) error {
	if err := requireAbsentRuntimeAccountFile(filepath.Join(profile, "session.json")); err != nil {
		return err
	}
	for _, database := range []string{"rsrs.db", "onememory.db"} {
		for _, suffix := range []string{"", "-wal", "-shm", "-journal"} {
			_, err := os.Lstat(filepath.Join(profile, database+suffix))
			if err == nil {
				return errors.New(i18n.Message7ccef183a8039380)
			}
			if !errors.Is(err, os.ErrNotExist) {
				return err
			}
		}
	}
	return nil
}

func (m *Manager) withStoppedKeygenLibrary(ctx context.Context, record runtimeOfflineKeygenRecord, work func() error) error {
	address, _, err := m.runtimeAccountEndpoint()
	if err != nil {
		return err
	}
	installation, err := m.lockRuntimeDirectory(ctx, record.Root)
	if err != nil {
		return err
	}
	defer installation.Close()
	if record.Root != record.Profile {
		library, err := m.lockRuntimeDirectory(ctx, record.Profile)
		if err != nil {
			return err
		}
		defer library.Close()
	}
	if err := requireStoppedRuntime(ctx, record.Root, address); err != nil {
		return err
	}
	selected, err := selectedRuntimeLoginProfile(record.Root)
	if err != nil {
		return err
	}
	if selected != record.Profile {
		return errors.New(i18n.Messagea23bc35adad0deaa)
	}
	return work()
}

func (record runtimeOfflineKeygenRecord) readOriginal(data []byte) (runtimeOfflineKeygenRecord, error) {
	var saved runtimeOfflineKeygenRecord
	if json.Unmarshal(data, &saved) != nil || saved.Schema != record.Schema || saved.Operation != record.Operation ||
		saved.Root != record.Root || saved.Source != record.Source || saved.Profile != record.Profile ||
		saved.Image != record.Image || saved.ImageDigest != record.ImageDigest || saved.Original != record.Original ||
		!hostOperationID.MatchString(saved.Token) || saved.Coordinator == "" || !validHash(saved.MarkerDigest) ||
		saved.Force || saved.WasServing || saved.StoreReady ||
		(saved.NextMarker != "" && (!validHash(saved.NextMarker) || saved.Stage != "sent")) ||
		(saved.Stage == "sent" && saved.Session != nil) ||
		(saved.Stage == "committed" && (saved.Session == nil || !validHash(*saved.Session))) ||
		(saved.Stage != "sent" && saved.Stage != "committed") {
		return saved, errors.New(i18n.Messagea327995cc4e43cff)
	}
	return saved, nil
}

func (record runtimeOfflineKeygenRecord) save(path string, original []byte) ([]byte, error) {
	data, err := json.Marshal(record)
	if err != nil {
		return nil, err
	}
	return data, writeRuntimeAccountRecord(path, data, original)
}

func offlineKeygenEnvelope(record runtimeOfflineKeygenRecord) (json.RawMessage, string, error) {
	raw, err := json.Marshal(map[string]any{"command": "keygen", "status": "ok", "summary": map[string]any{
		"operation_id": record.Operation, "outcome": "committed", "path": filepath.Join(record.Profile, "session.json"), "session_sha256": record.Session,
	}})
	return raw, "", err
}

func keygenIdentity(profile string) (runtimeAuthIdentity, error) {
	raw, err := readCredentialConfig(filepath.Join(profile, "session.json"), controlFrameLimit)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return runtimeAuthIdentity{}, err
	}
	defer clear(raw)
	return runtimeAuthIdentityOf(raw)
}

func offlineKeygenAlias(operation string) string {
	return "login-" + strings.ReplaceAll(operation, "-", "")
}

func (record runtimeOfflineKeygenRecord) verifySession() error {
	identity, err := keygenIdentity(record.Profile)
	if err != nil {
		return err
	}
	if record.Session == nil || identity.Kind != "unbound" || identity.OperationID != record.Operation ||
		identity.SessionDigest != *record.Session {
		return errors.New(i18n.Messagebd867e566ea4aa95)
	}
	return nil
}

func (m *Manager) exchangeOfflineKeygen(ctx context.Context, record runtimeOfflineKeygenRecord,
	marker []byte, material runtimeRegisterMaterial, identity runtimeAuthIdentity, method string) (string, error) {
	control, err := m.prepareRuntimeAccountControl(ctx, record.Image, record.ImageDigest, record.Profile, nil, marker)
	if err != nil {
		return "", err
	}
	requestID, err := controlUUID()
	if err != nil {
		return "", err
	}
	argument, err := json.Marshal(struct {
		Schema   int                     `json:"schema_version"`
		Binding  runtimeControlBinding   `json:"binding"`
		Identity runtimeAuthIdentity     `json:"identity"`
		Force    bool                    `json:"force"`
		Material runtimeRegisterMaterial `json:"material"`
	}{Schema: 1, Binding: control.binding, Identity: identity, Material: material})
	if err != nil {
		return "", err
	}
	defer clear(argument)
	reply, err := m.exchangeRuntimeControl(ctx, control.image, control.port, control.grant, runtimeControlRequest{
		V: 1, ID: requestID, Method: method, Args: []string{string(argument)},
		OperationID: record.Operation, IdempotencyKey: record.Operation,
	})
	defer clear(reply.Envelope)
	if err != nil {
		// The helper's private diagnostics may contain recovery material.
		return "", errors.New(i18n.Message31909f273c0aafac)
	}
	var envelope struct {
		Command string `json:"command"`
		Status  string `json:"status"`
		Summary struct {
			Operation string `json:"operation_id"`
			Outcome   string `json:"outcome"`
			Path      string `json:"path"`
			Digest    string `json:"session_sha256"`
			runtimeRegisterMaterial
		} `json:"summary"`
	}
	if json.Unmarshal(reply.Envelope, &envelope) != nil || envelope.Command != "keygen" ||
		envelope.Status != "ok" || envelope.Summary.Outcome != "committed" || envelope.Summary.Operation != record.Operation ||
		envelope.Summary.Path != filepath.Join(record.Profile, "session.json") || !validHash(envelope.Summary.Digest) ||
		subtle.ConstantTimeCompare([]byte(envelope.Summary.Super), []byte(material.Super)) != 1 {
		return "", errors.New(i18n.Message1572880a8ac70b23)
	}
	record.Session = &envelope.Summary.Digest
	return *record.Session, record.verifySession()
}
