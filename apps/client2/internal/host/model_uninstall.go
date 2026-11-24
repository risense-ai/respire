package host

import "respire-client2/internal/i18n"

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"runtime"
	"strings"
)

type m3Removal struct {
	root, profile, target, address string
	client, inference, registry    *[sha256.Size]byte
}

func modelConfigDigest(raw []byte) *[sha256.Size]byte {
	if raw == nil {
		return nil
	}
	digest := sha256.Sum256(raw)
	return &digest
}

func (m *Manager) removeM3UnderTakeover(ctx context.Context, takeover io.Closer, removal m3Removal, recheck func() error) (bool, error) {
	if m.hostAuthority == nil {
		return false, errRuntimeHostAuthorityUnavailable
	}
	if takeover == nil {
		return false, errors.New("Model uninstall requires Runtime takeover ownership")
	}
	if err := recheck(); err != nil {
		return false, err
	}
	if err := ctx.Err(); err != nil {
		return false, err
	}
	return m.hostAuthority.removeM3(ctx, takeover, removal)
}

func (m *Manager) uninstallM3(ctx context.Context) (json.RawMessage, error) {
	var result json.RawMessage
	err := m.RunTask(i18n.Messagec28b96a00f38a2b7, func() error {
		var err error
		result, err = m.uninstallM3Locked(ctx)
		return err
	})
	return result, err
}

func (m *Manager) uninstallM3Locked(ctx context.Context) (result json.RawMessage, err error) {
	address, _, err := m.runtimeAccountEndpoint()
	if err != nil {
		return nil, err
	}
	root := windowsDOSPath(m.DataRoot)
	if err := checkModelUninstallPath(root); err != nil {
		return nil, err
	}
	takeover, err := m.lockRuntimeTakeover(ctx)
	if err != nil {
		return nil, err
	}
	defer func() {
		if takeover != nil {
			err = errors.Join(err, takeover.Close())
		}
	}()

	h, _, err := m.Health(ctx)
	running := err == nil
	if !running && !IsUnavailable(err) {
		return nil, fmt.Errorf(i18n.Message866bf9cf5010c78f, err)
	}
	if running && h.ManagedID != "" {
		return nil, errors.New(i18n.Message901efb3766fb7242)
	}
	if err := checkM3UninstallAdmission(root); err != nil {
		return nil, err
	}

	clientPath := filepath.Join(root, "client.json")
	client, err := modelUninstallObject(clientPath)
	if err != nil {
		return nil, err
	}
	profile, err := modelUninstallProfile(root, client)
	if err != nil {
		return nil, err
	}
	target := windowsDOSPath(m.ModelDir())
	if err := checkM3UninstallTarget(target, profile); err != nil {
		return nil, err
	}
	inference, err := modelUninstallObject(filepath.Join(root, "inference.json"))
	if err != nil {
		return nil, err
	}
	registry, err := modelUninstallObject(filepath.Join(root, "embedding-profiles-v1.json"))
	if err != nil {
		return nil, err
	}
	recheck := func() error {
		if err := requireStoppedM3Runtime(ctx, root, address); err != nil {
			return err
		}
		if err := checkModelUninstallPath(profile); err != nil {
			return err
		}
		for _, input := range []struct {
			name string
			data []byte
		}{{"client.json", client}, {"inference.json", inference}, {"embedding-profiles-v1.json", registry}} {
			now, err := modelUninstallFile(filepath.Join(root, input.name))
			if err != nil {
				return err
			}
			if !bytes.Equal(now, input.data) || (now == nil) != (input.data == nil) {
				return fmt.Errorf(i18n.Messagedcb2240d0ae77e47, input.name)
			}
		}
		if err := checkM3UninstallAdmission(root); err != nil {
			return err
		}
		return checkM3UninstallTarget(target, profile)
	}

	removal := m3Removal{
		root: root, profile: profile, target: target, address: address,
		client: modelConfigDigest(client), inference: modelConfigDigest(inference), registry: modelConfigDigest(registry),
	}
	var installed InstalledRuntime
	if running {
		installed, err = m.verifyDirectM3Runtime(ctx, h, profile)
		if err != nil {
			return nil, err
		}
		if err := m.checkLiveM3Assets(ctx, h, target); err != nil {
			return nil, err
		}
		if err := m.stopDetached(ctx, h); err != nil {
			return nil, fmt.Errorf(i18n.Messageaab7882442f7dbc2, err)
		}
	}

	removed, err := m.removeM3UnderTakeover(ctx, takeover, removal, recheck)
	if err != nil && !errors.Is(err, errM3Active) {
		if running {
			return nil, fmt.Errorf(i18n.Messagee87e97898c0f1de8, err)
		}
		return nil, err
	}
	removalErr := err
	if removalErr == nil {
		status, message := "skip", "not installed"
		if removed {
			status, message = "ok", target
		}
		result, err = json.Marshal(map[string]any{
			"command": "model", "status": status,
			"summary": map[string]any{"dir": target, "removed": removed, "model": "bge-m3"},
			"items":   []map[string]string{{"name": "model", "status": status, "value": message}},
		})
		if err != nil {
			return nil, err
		}
	}
	if running {
		// Retain takeover through restart and check that the same account returns.
		restored, err := m.startDetachedLocked(ctx, installed)
		if err == nil && (!m.isDirectM3Runtime(restored) || !samePath(restored.DataDir, profile) || restored.AccountID != h.AccountID ||
			restored.InstanceID == h.InstanceID) {
			err = errors.New(i18n.Messagef2c6bd21dffcbee9)
		}
		if err == nil {
			err = takeover.Close()
			takeover = nil
			if err == nil {
				err = m.unlockIfNeeded(restored)
			}
		}
		if err == nil && h.StoreReady {
			current, _, healthErr := m.Health(ctx)
			err = healthErr
			if err == nil && (!current.StoreReady || current.AccountID != h.AccountID || current.InstanceID != restored.InstanceID) {
				err = errors.New(i18n.Message1841429a7d4e4063)
			}
		}
		if err != nil {
			if removalErr != nil {
				return nil, errors.Join(removalErr, fmt.Errorf(i18n.ModelUninstallRestoreFailed, err))
			}
			return result, fmt.Errorf(i18n.Message85ff556df8993091, err)
		}
	}
	return result, removalErr
}

func (m *Manager) isDirectM3Runtime(h HealthInfo) bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	owner := m.directRuntime
	return owner.PID != 0 && owner.PID == h.PID &&
		owner.InstanceID == h.InstanceID && owner.ProcessStartID == h.ProcessStartID
}

func (m *Manager) verifyDirectM3Runtime(ctx context.Context, h HealthInfo, profile string) (InstalledRuntime, error) {
	if !m.isDirectM3Runtime(h) {
		return InstalledRuntime{}, errors.New(i18n.Message3ce8f19d590f629c)
	}
	info, err := m.Installed()
	if err != nil {
		return info, err
	}
	if h.RuntimeVersion != info.Version || !h.StartupComplete ||
		!samePath(h.DataDir, profile) || !strings.EqualFold(h.ImageSHA256, info.SHA256) ||
		!samePath(h.Exe, h.ImagePath) {
		return info, errors.New(i18n.Message8fa2470c290ff6c0)
	}
	if err := m.verifyInstalled(info); err != nil {
		return info, err
	}
	if err := m.verifySelectedHealth(h, info); err != nil {
		return info, err
	}
	birth, err := runtimeControlProcessBirth(ctx, int(h.PID))
	if err != nil {
		return info, err
	}
	if birth == "" || h.ProcessStartID != runtimeBirthID(birth) {
		return info, errors.New(i18n.Message00eb2287cd80578c)
	}
	return info, nil
}

func runtimeBirthID(birth string) string {
	switch runtime.GOOS {
	case "windows":
		return "windows-filetime-" + birth
	case "darwin":
		return "darwin-lstart-" + birth
	default:
		return "linux-starttime-" + birth
	}
}

func checkM3UninstallAdmission(root string) error {
	marker := filepath.Join(root, "runtime", "autostart", "maintenance.json")
	if err := checkModelUninstallPath(marker); err != nil {
		return err
	}
	if _, err := os.Lstat(marker); err == nil {
		return errors.New(i18n.Message3720981ebe4c8151)
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	return nil
}

func requireStoppedM3Runtime(ctx context.Context, root, address string) error {
	if err := requireClosedRuntimePort(ctx, address); err != nil {
		return err
	}
	// The caller holds the actual installation and library locks. A historical
	// endpoint can add proof of a live owner, but cannot establish one by itself.
	raw, err := modelUninstallFile(filepath.Join(root, "runtime", "endpoint.json"))
	var endpoint HealthInfo
	if err != nil || json.Unmarshal(raw, &endpoint) != nil ||
		endpoint.PID == 0 || endpoint.ProcessStartID == "" || endpoint.Server != "respire" ||
		!samePath(endpoint.InstallationRoot, root) || !filepath.IsAbs(endpoint.Exe) ||
		!samePath(endpoint.Exe, endpoint.ImagePath) || !validHash(endpoint.ImageSHA256) {
		return nil
	}
	birth, err := runtimeControlProcessBirth(ctx, int(endpoint.PID))
	if err != nil || birth == "" || runtimeBirthID(birth) != endpoint.ProcessStartID ||
		verifyHostProcess(endpoint) != nil {
		return ctx.Err()
	}
	hash, err := fileHash(endpoint.Exe)
	if err != nil || !strings.EqualFold(hash, endpoint.ImageSHA256) {
		return ctx.Err()
	}
	birth, err = runtimeControlProcessBirth(ctx, int(endpoint.PID))
	if err == nil && birth != "" && runtimeBirthID(birth) == endpoint.ProcessStartID {
		return errors.New(i18n.Messageada9194429241ab9)
	}
	return ctx.Err()
}
