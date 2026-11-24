package host

import "respire-client2/internal/i18n"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"time"
)

// ErrRuntimeNotInstalled means there is no selected Runtime to start.
var ErrRuntimeNotInstalled = errors.New(i18n.Messageae18a43c0b16d7be)

var errInvalidRuntimeSelection = errors.New("invalid installed Runtime metadata")

func (m *Manager) Installed() (InstalledRuntime, error) {
	var info InstalledRuntime
	b, err := os.ReadFile(m.runtimeManifest())
	if err != nil {
		return info, err
	}
	if err = json.Unmarshal(b, &info); err != nil {
		return info, fmt.Errorf("%w: %w", errInvalidRuntimeSelection, err)
	}
	if !validVersion(info.Version) || !validHash(info.SHA256) {
		return info, errInvalidRuntimeSelection
	}
	return info, nil
}

func (m *Manager) runtimeDir(v string) string { return filepath.Join(m.DataRoot, "bin", "runtime", v) }

func (m *Manager) Owns(exe string) bool {
	for _, root := range []string{filepath.Join(m.DataRoot, "bin", "runtime"), filepath.Join(m.DataRoot, "runtime"), filepath.Join(m.Root, "runtime")} {
		rel, err := filepath.Rel(windowsDOSPath(root), windowsDOSPath(exe))
		if err == nil && filepath.IsLocal(rel) && acceptsRuntimeBinary(filepath.Base(rel)) {
			return true
		}
	}
	return false
}

func (m *Manager) Start() error {
	return m.start(context.Background(), true)
}

// PrepareRuntimeForStartup reuses a running Runtime or starts the verified local selection.
func (m *Manager) PrepareRuntimeForStartup(ctx context.Context) error {
	return m.start(ctx, false)
}

func (m *Manager) start(ctx context.Context, switchRunning bool) error {
	healthCtx, cancel := context.WithTimeout(ctx, 3*time.Second)
	h, _, err := m.Health(healthCtx)
	cancel()
	if err == nil {
		if switchRunning && m.Owns(h.Exe) {
			info, installedErr := m.Installed()
			if installedErr != nil {
				return fmt.Errorf(i18n.Message2bbd1cffcae9262b, installedErr)
			}
			if !samePath(h.Exe, filepath.Join(m.runtimeDir(info.Version), runtimeBinary())) {
				if err := m.Stop(true); err != nil {
					return err
				}
				return m.Start()
			}
		}
		return m.unlockIfNeeded(h)
	}
	if !IsUnavailable(err) {
		return fmt.Errorf(i18n.Message784369e8fe658eeb, err)
	}
	info, err := m.Installed()
	if errors.Is(err, os.ErrNotExist) {
		return fmt.Errorf("%w: %w", ErrRuntimeNotInstalled, err)
	}
	if err != nil {
		return fmt.Errorf(i18n.Message2bbd1cffcae9262b, err)
	}
	ctx, cancel = context.WithTimeout(ctx, 90*time.Second)
	defer cancel()
	h, err = m.startDetached(ctx, info)
	if err != nil {
		return err
	}
	return m.unlockIfNeeded(h)
}

func (m *Manager) verifySelectedHealth(h HealthInfo, info InstalledRuntime) error {
	if h.Version != info.Version || !samePath(h.Exe, filepath.Join(m.runtimeDir(info.Version), runtimeBinary())) ||
		!samePath(h.InstallationRoot, m.DataRoot) {
		return errors.New(i18n.Message527f3396d679a8ac)
	}
	if err := verifyHostProcess(h); err != nil {
		return fmt.Errorf(i18n.Messaged52118f166203327, err)
	}
	return nil
}

func (m *Manager) unlockIfNeeded(health HealthInfo) error {
	if health.StoreReady || health.CredentialState != "required" {
		return nil
	}
	if samePath(health.InstallationRoot, m.DataRoot) && filepath.IsAbs(health.DataDir) {
		relative, err := filepath.Rel(m.DataRoot, health.DataDir)
		if err == nil && filepath.IsLocal(relative) {
			// An accountless Runtime can serve its login and status endpoints while locked.
			if _, err := os.Lstat(filepath.Join(health.DataDir, "session.json")); errors.Is(err, os.ErrNotExist) {
				return nil
			} else if err != nil {
				return err
			}
		}
	}
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	return m.unlockAccountRuntime(ctx, health)
}

func (m *Manager) Stop(ownedOnly bool) error {
	ctx, cancel := context.WithTimeout(context.Background(), 35*time.Second)
	defer cancel()
	h, _, err := m.Health(ctx)
	if err != nil {
		if IsUnavailable(err) {
			return nil
		}
		return err
	}
	if ownedOnly && !m.Owns(h.Exe) {
		return errors.New(i18n.Message44d4204db8b80209)
	}
	return m.stopManaged(ctx, h)
}
