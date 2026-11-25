//go:build linux || windows || (darwin && cgo)

package host

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

func (m *Manager) desktopOfflineKeygen(ctx context.Context, id string, query bool) (json.RawMessage, string, error) {
	return m.desktopOfflineKeygenWithStore(ctx, id, query, nativeRuntimeCredentialStore)
}

func (m *Manager) desktopOfflineKeygenWithStore(ctx context.Context, id string, query bool,
	openStore func(context.Context) hostCredentialStore) (raw json.RawMessage, recovery string, err error) {
	op, ok := strings.CutPrefix(id, "desktop-")
	if !ok || !hostOperationID.MatchString(op) || m.ClientVersion == "" {
		return nil, "", errors.New("Invalid desktop cold keygen operation")
	}
	defer func() {
		var already *unconfirmedError
		if query && err != nil && !errors.As(err, &already) {
			err = &unconfirmedError{id: id, cause: err}
		}
	}()
	ctx, cancel := context.WithTimeout(ctx, 90*time.Second)
	defer cancel()

	installed, err := m.Installed()
	if err != nil {
		return nil, "", err
	}
	binding, err := m.selectedColdKeygenBinding(installed, op)
	if err != nil {
		return nil, "", err
	}
	custody, err := m.loadColdKeygenCustody(binding)
	switch {
	case errors.Is(err, os.ErrNotExist) && query:
		legacy, legacyErr := m.hasLegacyColdKeygenCheckpoint(binding)
		if legacyErr != nil {
			return nil, "", legacyErr
		}
		if legacy {
			return m.recoverLegacyOfflineKeygen(ctx, id)
		}
		return nil, "", &unconfirmedError{id: id, cause: errors.New("Original cold keygen custody checkpoint not found")}
	case errors.Is(err, os.ErrNotExist):
	case err != nil:
		return nil, "", &unconfirmedError{id: id, cause: err}
	case !query || !custody.Sent:
		return nil, "", &unconfirmedError{id: id, cause: errors.New("Only a sent original cold keygen can be queried")}
	}

	grant := func() (runtimeAccountControl, error) {
		current, err := m.selectedColdKeygenBinding(installed, op)
		if err != nil {
			return runtimeAccountControl{}, err
		}
		if current != binding {
			return runtimeAccountControl{}, errors.New("Selected cold keygen scope changed")
		}
		return m.prepareRuntimeControlGrant(ctx, binding.Image, binding.ImageSHA, binding.Profile, nil, nil, "Paused")
	}
	probe, err := grant()
	if err != nil {
		return nil, "", err
	}
	if err := m.probeColdKeygen(ctx, probe); err != nil {
		return nil, "", err
	}

	store := openStore(ctx)
	if query {
		return m.finishNativeColdKeygen(id, custody, true, store, func(material runtimeRegisterMaterial) (string, error) {
			control, err := grant()
			if err != nil {
				return "", err
			}
			return m.exchangeColdKeygen(ctx, control, op, true, material)
		})
	}

	if err := requireUninitializedKeygenProfile(binding.Profile); err != nil {
		return nil, "", err
	}
	if err := m.stopAccountlessColdKeygenRuntime(ctx, binding); err != nil {
		return nil, "", err
	}
	if err := requireUninitializedKeygenProfile(binding.Profile); err != nil {
		return nil, "", err
	}
	material, err := newRuntimeVaultMaterial(op)
	if err != nil {
		return nil, "", err
	}
	defer func() { material.Super = "" }()
	if present, err := store.readSlot(material.Backend, "rsrs", "super:"+material.Account, true); err != nil || present != nil {
		return nil, "", errors.New("Cold keygen credential slot is not available")
	}
	custody, err = m.prepareColdKeygenCustody(binding, material)
	if err != nil {
		return nil, "", err
	}
	if err := store.preserve("rsrs", "super:"+material.Account, material.Super); err != nil {
		return nil, "", &unconfirmedError{id: id, cause: errors.New("Cold keygen credential custody unconfirmed")}
	}
	return m.finishNativeColdKeygen(id, custody, false, store, func(original runtimeRegisterMaterial) (string, error) {
		if subtle.ConstantTimeCompare([]byte(material.Super), []byte(original.Super)) != 1 {
			return "", errors.New("Cold keygen credential slot changed")
		}
		control, err := grant()
		if err != nil {
			return "", err
		}
		if err := m.checkpointColdKeygenSend(custody); err != nil {
			return "", err
		}
		return m.exchangeColdKeygen(ctx, control, op, false, original)
	})
}

func (m *Manager) selectedColdKeygenBinding(installed InstalledRuntime, op string) (coldKeygenCustodyBinding, error) {
	if err := m.verifyRuntimeSelection(installed); err != nil {
		return coldKeygenCustodyBinding{}, err
	}
	root, err := canonicalSessionDirectory(m.DataRoot)
	if err != nil {
		return coldKeygenCustodyBinding{}, err
	}
	profile, err := selectedRuntimeLoginProfile(root)
	if err != nil {
		return coldKeygenCustodyBinding{}, err
	}
	image, err := canonicalSessionDirectory(filepath.Join(m.runtimeDir(installed.Version), runtimeBinary()))
	if err != nil {
		return coldKeygenCustodyBinding{}, err
	}
	return coldKeygenCustodyBinding{Operation: op, Profile: profile, Image: image, ImageSHA: installed.SHA256}, nil
}

func (m *Manager) hasLegacyColdKeygenCheckpoint(binding coldKeygenCustodyBinding) (bool, error) {
	root, err := canonicalSessionDirectory(m.DataRoot)
	if err != nil {
		return false, err
	}
	dir := filepath.Join(root, "runtime", "autostart")
	if err := checkRuntimeAccountDirectory(dir); errors.Is(err, os.ErrNotExist) {
		return false, nil
	} else if err != nil {
		return false, err
	}
	for _, name := range []string{"offline-keygen-result-" + binding.Operation + ".json", offlineKeygenFile} {
		raw, err := readCredentialConfig(filepath.Join(dir, name), runtimeMaintenanceLimit)
		if errors.Is(err, os.ErrNotExist) {
			continue
		}
		if err != nil {
			return false, err
		}
		base := runtimeOfflineKeygenRecord{Schema: 1, Operation: binding.Operation, Root: root, Source: binding.Profile,
			Profile: binding.Profile, Image: binding.Image, ImageDigest: binding.ImageSHA, Original: runtimeAuthIdentity{Kind: "absent"}}
		if _, err := base.readOriginal(raw); err != nil {
			return false, err
		}
		return true, nil
	}
	return false, nil
}

func (m *Manager) stopAccountlessColdKeygenRuntime(ctx context.Context, binding coldKeygenCustodyBinding) error {
	h, _, err := m.Health(ctx)
	if IsUnavailable(err) {
		// The child independently acquires startup + serving + auth exclusion.
		return nil
	}
	if err != nil {
		return err
	}
	if h.ManagedID != "" || h.StoreReady || !samePath(h.DataDir, binding.Profile) || !samePath(h.ImagePath, binding.Image) ||
		h.ImageSHA256 != binding.ImageSHA || h.AccountID != controlDigest([]byte(binding.Profile)) {
		return errors.New("Cold keygen cannot stop a different or initialized Runtime")
	}
	return m.stopSelectedDetached(ctx, h)
}

func (m *Manager) finishNativeColdKeygen(id string, custody coldKeygenCustody, query bool, store hostCredentialStore,
	exchange func(runtimeRegisterMaterial) (string, error)) (json.RawMessage, string, error) {
	unconfirmed := func(cause error) (json.RawMessage, string, error) {
		return nil, "", &unconfirmedError{id: id, cause: cause}
	}
	binding := custody.Intent.Binding
	wrapped := custody.Intent.Material
	factor, err := store.readSlot(wrapped.Backend, "rsrs", "super:"+wrapped.Account, false)
	if err != nil || factor == nil || *factor == "" ||
		(exportVault{Salt: wrapped.Salt, Wrapped: wrapped.Wrapped, Nonce: wrapped.Nonce}).verifyExportSuper(*factor) != nil {
		return unconfirmed(errors.New("Original cold keygen credential could not be verified"))
	}
	digest, err := exchange(runtimeRegisterMaterial{Account: wrapped.Account, Backend: wrapped.Backend,
		Salt: wrapped.Salt, Wrapped: wrapped.Wrapped, Nonce: wrapped.Nonce, Super: *factor})
	if err != nil {
		return unconfirmed(errors.New("Original cold keygen result is unconfirmed"))
	}
	record := runtimeOfflineKeygenRecord{Operation: binding.Operation, Profile: binding.Profile, Session: &digest}
	if err := record.verifySession(); err != nil {
		return unconfirmed(err)
	}
	raw, _, err := offlineKeygenEnvelope(record)
	if err != nil {
		return unconfirmed(err)
	}
	if query {
		return raw, "", nil
	}
	return raw, *factor, nil
}
