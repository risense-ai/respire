package host

import "respire-client2/internal/i18n"

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"time"
)

func (m *Manager) installModelEngines(ctx context.Context) (result json.RawMessage, err error) {
	if err := m.requireTakeoverProfile(); err != nil {
		return nil, err
	}
	catalog, err := m.downloadWinMLCatalog(ctx)
	if err != nil {
		return nil, err
	}
	install, err := m.lockRuntime(ctx)
	if err != nil {
		return nil, err
	}
	defer func() { err = errors.Join(err, install.Close()) }()

	takeover, err := m.lockRuntimeTakeover(ctx)
	if err != nil {
		return nil, err
	}
	root, err := canonicalSessionDirectory(m.DataRoot)
	if err != nil {
		return nil, errors.Join(err, takeover.Close())
	}
	markerPath := filepath.Join(root, "runtime", "autostart", "maintenance.json")
	var marker []byte
	var info InstalledRuntime
	var previous HealthInfo
	var profile string
	stopped := false
	defer func() {
		// The private child must have exited before we release Paused authority.
		if err != nil && strings.Contains(err.Error(), "exit unconfirmed") {
			err = &unconfirmedError{cause: errors.Join(err, takeover.Close())}
			return
		}
		if marker != nil {
			if removeErr := removeOwnedRuntimeAccountRecord(markerPath, marker); removeErr != nil {
				err = &unconfirmedError{cause: errors.Join(err, removeErr, takeover.Close())}
				return
			}
		}
		if !stopped {
			err = errors.Join(err, takeover.Close())
			return
		}
		restart, cancel := context.WithTimeout(context.WithoutCancel(ctx), 120*time.Second)
		defer cancel()
		active, restoreErr := m.startDetachedLocked(restart, info)
		restoreErr = errors.Join(restoreErr, takeover.Close())
		if restoreErr == nil {
			restoreErr = m.confirmProviderRuntime(restart, info, previous, active, profile)
		}
		if restoreErr != nil {
			err = errors.Join(err, fmt.Errorf(i18n.Message62202a2bfb7b7444, restoreErr))
		}
	}()
	if err := m.requireNoDeviceMaintenance(); err != nil {
		return nil, err
	}
	info, err = m.Installed()
	if err != nil {
		return nil, err
	}
	if err := m.verifyInstalled(info); err != nil {
		return nil, err
	}
	config, selected, err := m.readDeviceConfig()
	if err != nil {
		return nil, err
	}
	profile, err = canonicalSessionDirectory(selected.profile)
	if err != nil {
		return nil, err
	}
	identity, err := deviceProfileIdentity(profile)
	if err != nil {
		return nil, err
	}
	address, port, err := m.runtimeAccountEndpoint()
	if err != nil {
		return nil, err
	}
	directory, err := deviceProfilePath(filepath.Dir(markerPath))
	if err != nil {
		return nil, err
	}
	coordinator, err := providerMaintenanceCoordinator(directory, root, port)
	if err != nil {
		return nil, err
	}
	if m.ClientVersion == "" {
		return nil, errors.New(i18n.Message9fee1bddb224f3f8)
	}
	birth, err := hostControlBirth(ctx)
	if err != nil {
		return nil, err
	}
	token, err := controlUUID()
	if err != nil {
		return nil, err
	}
	probe, cancel := context.WithTimeout(ctx, 3*time.Second)
	active, _, probeErr := m.Health(probe)
	cancel()
	if probeErr == nil {
		if active.ManagedID != "" {
			return nil, errors.New(i18n.Messagedde6cfe3b284325a)
		}
		if err := m.verifyDeviceConfigOwner(ctx, active, info, profile); err != nil {
			return nil, err
		}
		drain, cancel := context.WithTimeout(ctx, 35*time.Second)
		err := m.stopDetached(drain, active)
		cancel()
		if err != nil {
			return nil, err
		}
		previous, stopped = active, true
	} else if !IsUnavailable(probeErr) {
		return nil, probeErr
	}
	if err := m.archiveStoppedEndpoint(ctx, address); err != nil {
		return nil, err
	}
	if err := os.MkdirAll(directory, 0700); err != nil {
		return nil, err
	}
	paused, err := json.Marshal(runtimeMaintenanceMarker{
		Schema: 1, ID: coordinator, Token: token, Phase: "Paused", ExpectedProfile: profile,
		ExpectedVersion: m.ClientVersion, OwnerPID: os.Getpid(), OwnerBirth: birth,
	})
	if err != nil {
		return nil, err
	}
	configLock, err := lockDeviceConfig(ctx, filepath.Join(root, "inference.json"))
	if err != nil {
		return nil, err
	}
	defer func() { err = errors.Join(err, configLock.Close()) }()
	if err := m.withProviderLibraryLocks(ctx, root, profile, func() error {
		if err := requireClosedRuntimePort(ctx, address); err != nil {
			return err
		}
		if err := writeRuntimeAccountRecord(markerPath, paused, nil); err != nil {
			return err
		}
		marker = paused
		return nil
	}); err != nil {
		return nil, err
	}
	checkBinding := func() error {
		currentMarker, err := readCredentialConfig(markerPath, runtimeMaintenanceLimit)
		if err != nil || !bytes.Equal(currentMarker, marker) {
			return errors.Join(errors.New(i18n.Message60eb3cc32a2e9691), err)
		}
		current, _, err := m.readDeviceConfig()
		if err != nil || !bytes.Equal(current, config) {
			return errors.Join(errors.New(i18n.Message9620de645739b653), err)
		}
		currentIdentity, err := deviceProfileIdentity(profile)
		if err != nil {
			return err
		}
		if currentIdentity.Kind != identity.Kind || currentIdentity.SessionDigest != identity.SessionDigest {
			return errors.New(i18n.Message70d7d21dfc763f8b)
		}
		currentRuntime, err := m.Installed()
		if err != nil || currentRuntime != info {
			return errors.Join(errors.New(i18n.Message4e638a2164086715), err)
		}
		return nil
	}
	if err := checkBinding(); err != nil {
		return nil, err
	}
	plan, err := m.providerPreparation(ctx, info, profile, identity.AccountID, marker, "")
	if err != nil {
		return nil, err
	}
	if !plan.matchesCatalog(root, catalog, false) {
		return nil, errors.New(i18n.Messagea93cd243192558d8)
	}
	if err := m.withProviderLibraryLocks(ctx, root, profile, func() error {
		if err := checkBinding(); err != nil {
			return err
		}
		// WinML's dynamic provider downloads have no pin in the current contract.
		return catalog.publish(root)
	}); err != nil {
		return nil, err
	}
	verified, err := m.providerPreparation(ctx, info, profile, identity.AccountID, marker, plan.Selection)
	if err != nil {
		return nil, fmt.Errorf(i18n.Messagedd3167cf121a1cb0, err)
	}
	if err := checkBinding(); err != nil {
		return nil, err
	}
	if !verified.matchesCatalog(root, catalog, true) {
		return nil, errors.New(i18n.Messagef470d6bdab3ba714)
	}
	return providerInstallReceipt(verified)
}

func providerMaintenanceCoordinator(directory, root string, port uint64) (string, error) {
	raw, err := readCredentialConfig(filepath.Join(directory, "registration.json"), controlFrameLimit)
	if errors.Is(err, os.ErrNotExist) {
		return runtimeRegistrationCoordinator(root, nil)
	}
	if err != nil {
		return "", err
	}
	var policy struct {
		Enabled         *bool `json:"enabled"`
		ManuallyStopped *bool `json:"manually_stopped"`
	}
	id := deviceConfigRegistrationID(raw, root, port)
	if id == "" || json.Unmarshal(raw, &policy) != nil || policy.Enabled == nil || policy.ManuallyStopped == nil {
		return "", errors.New(i18n.Messagec4dd164f47b47e26)
	}
	if *policy.Enabled && !*policy.ManuallyStopped {
		return "", errors.New(i18n.Messagedde6cfe3b284325a)
	}
	return id, nil
}

func (m *Manager) withProviderLibraryLocks(ctx context.Context, root, profile string, action func() error) (err error) {
	runDir := filepath.Join(root, "runtime")
	for _, dir := range []string{runDir, root} {
		lock, err := m.lockRuntimeDirectory(ctx, dir)
		if err != nil {
			return err
		}
		defer func() { err = errors.Join(err, lock.Close()) }()
	}
	if !samePath(profile, root) && !samePath(profile, runDir) {
		lock, err := m.lockRuntimeDirectory(ctx, profile)
		if err != nil {
			return err
		}
		defer func() { err = errors.Join(err, lock.Close()) }()
	}
	return action()
}

func (m *Manager) confirmProviderRuntime(ctx context.Context, version InstalledRuntime, previous, active HealthInfo, profile string) error {
	if err := m.verifyDeviceConfigOwner(ctx, active, version, profile); err != nil {
		return err
	}
	if active.InstanceID == previous.InstanceID || active.AccountID != previous.AccountID || active.ManagedID != "" {
		return errors.New(i18n.Message324bc957d4318600)
	}
	if err := m.unlockIfNeeded(active); err != nil {
		return err
	}
	if previous.StoreReady && !active.StoreReady {
		current, _, err := m.Health(ctx)
		if err != nil {
			return err
		}
		if err := m.verifyDeviceConfigOwner(ctx, current, version, profile); err != nil {
			return err
		}
		if current.InstanceID != active.InstanceID || !current.StoreReady {
			return errors.New(i18n.Messagef25c7efaaa2c3565)
		}
	}
	return nil
}
