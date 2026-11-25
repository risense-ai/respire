package host

import (
	"context"
	"errors"
	"path/filepath"
	"respire-client2/internal/i18n"
	"strconv"
	"time"
)

func (m *Manager) verifyScopedRuntimeOwner(ctx context.Context, health HealthInfo, info InstalledRuntime, profile, account string) error {
	if err := m.verifySelectedHealth(health, info); err != nil {
		return err
	}
	birth, err := runtimeControlProcessBirth(ctx, int(health.PID))
	if err != nil {
		return err
	}
	if birth == "" || health.ProcessStartID != runtimeBirthID(birth) {
		return errors.New(i18n.Message82e68dcc37834b71)
	}
	root, err := canonicalSessionDirectory(m.DataRoot)
	if err != nil {
		return err
	}
	profile, err = canonicalSessionDirectory(profile)
	if err != nil {
		return err
	}
	if !health.StartupComplete || health.ImageSHA256 != info.SHA256 ||
		!samePath(health.ImagePath, health.Exe) || !samePath(health.DataDir, profile) ||
		health.InstallationID != controlDigest([]byte(root)) || health.AccountID != account ||
		!runtimeInstanceID.MatchString(health.InstanceID) || health.Generation == 0 {
		return errors.New(i18n.Message34a4deabb8b33b2c)
	}
	return nil
}

func (m *Manager) startWithStartupControl(ctx context.Context, info InstalledRuntime) (health HealthInfo, err error) {
	ctx, cancel := context.WithTimeout(ctx, 90*time.Second)
	defer cancel()
	guard, err := m.startupControl(ctx, info)
	if err != nil {
		return HealthInfo{}, err
	}
	defer func() { err = errors.Join(err, guard.Close()) }()
	held := guard.Held()
	return m.startDetachedComplete(guard.workCtx, info, func(h HealthInfo) error {
		if err := m.verifyRuntimeSelection(info); err != nil {
			return err
		}
		root, profile, account, err := m.startupScope()
		if err != nil {
			return err
		}
		if !samePath(root, held.InstallationRoot) || !samePath(profile, held.Profile) || account != held.AccountID {
			return errors.New("Startup scope changed while exclusion was held")
		}
		if err := m.verifyScopedRuntimeOwner(ctx, h, info, profile, account); err != nil {
			return err
		}
		image, err := canonicalSessionDirectory(filepath.Join(m.runtimeDir(info.Version), runtimeBinary()))
		if err != nil {
			return err
		}
		if !samePath(h.ImagePath, image) {
			return errors.New("Startup Runtime image mismatch")
		}
		_, port, err := m.runtimeAccountEndpoint()
		if err != nil {
			return err
		}
		endpoint := startupEndpoint{h.PID, h.ProcessStartID, h.ImagePath, h.ImageSHA256,
			h.InstanceID, h.AccountID, h.Generation, "http://127.0.0.1:" + strconv.FormatUint(port, 10)}
		_, err = guard.Finish(&endpoint)
		return err
	})
}
