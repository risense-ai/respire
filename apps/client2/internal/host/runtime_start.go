package host

import "respire-client2/internal/i18n"

import (
	"context"
	"errors"
	"fmt"
	"net/url"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
	"time"
)

func (m *Manager) startDetached(ctx context.Context, info InstalledRuntime) (HealthInfo, error) {
	release, err := m.holdProfile()
	if err != nil {
		return HealthInfo{}, err
	}
	defer release()
	// Report invalid local selections before requesting takeover ownership.
	if err := m.verifyRuntimeSelection(info); err != nil {
		return HealthInfo{}, err
	}
	if m.hostAuthority == nil && m.startupControl != nil {
		return m.startWithStartupControl(ctx, info)
	}
	lock, err := m.lockRuntimeTakeover(ctx)
	if err != nil {
		return HealthInfo{}, err
	}
	defer lock.Close()
	return m.startDetachedLocked(ctx, info)
}

func (m *Manager) verifyRuntimeSelection(info InstalledRuntime) error {
	selected, err := m.Installed()
	if err != nil {
		return err
	}
	if selected != info {
		return errors.New(i18n.Message22ae1f5488eb20c7)
	}
	return m.verifyInstalled(info)
}

func (m *Manager) startDetachedLocked(ctx context.Context, info InstalledRuntime) (HealthInfo, error) {
	return m.startDetachedComplete(ctx, info, nil)
}

func (m *Manager) startDetachedComplete(ctx context.Context, info InstalledRuntime, complete func(HealthInfo) error) (HealthInfo, error) {
	release, err := m.holdProfile()
	if err != nil {
		return HealthInfo{}, err
	}
	defer release()
	if err := m.verifyRuntimeSelection(info); err != nil {
		return HealthInfo{}, err
	}
	h, _, err := m.Health(ctx)
	if err == nil {
		if err := m.verifySelectedHealth(h, info); err != nil {
			return HealthInfo{}, err
		}
		if complete != nil {
			if err := complete(h); err != nil {
				return HealthInfo{}, err
			}
		}
		return h, nil
	}
	if !IsUnavailable(err) {
		return HealthInfo{}, err
	}
	endpoint, err := url.Parse(m.Endpoint)
	if err != nil {
		return HealthInfo{}, err
	}
	if endpoint.Scheme != "http" || endpoint.Hostname() != "127.0.0.1" || endpoint.Port() == "" {
		return HealthInfo{}, errors.New(i18n.Message95673ba51c33f3cd)
	}
	if err := requireAbsentRuntimeAccountFile(filepath.Join(m.DataRoot, "runtime", "autostart", "offline-keygen.json")); err != nil {
		return HealthInfo{}, fmt.Errorf(i18n.Messaged9c4a7410a734011, err)
	}
	if complete != nil {
		// The startup helper grants no authority to retire account maintenance.
		if err := m.requireNoDeviceMaintenance(); err != nil {
			return HealthInfo{}, fmt.Errorf("Startup exclusion cannot retire account maintenance: %w", err)
		}
	} else if err := archiveTerminalRuntimeMaintenance(m.DataRoot); err != nil {
		return HealthInfo{}, err
	}
	if err := m.archiveStoppedEndpoint(ctx, endpoint.Host); err != nil {
		return HealthInfo{}, err
	}
	executable := filepath.Join(m.runtimeDir(info.Version), runtimeBinary())
	cmd := runtimeServingCommand(executable, m.DataRoot, endpoint.Hostname(), endpoint.Port(), "")
	return m.launchRuntimeServingComplete(ctx, cmd, func(health HealthInfo) error {
		if health.ImageSHA256 != info.SHA256 {
			return errors.New(i18n.Message35bb3c888073700d)
		}
		if err := m.verifySelectedHealth(health, info); err != nil {
			return err
		}
		if complete != nil {
			return complete(health)
		}
		return nil
	})
}

func (m *Manager) launchRuntimeServing(ctx context.Context, cmd *exec.Cmd) (HealthInfo, error) {
	return m.launchRuntimeServingComplete(ctx, cmd, nil)
}

func (m *Manager) launchRuntimeServingComplete(ctx context.Context, cmd *exec.Cmd, complete func(HealthInfo) error) (result HealthInfo, resultErr error) {
	if err := ctx.Err(); err != nil {
		return HealthInfo{}, err
	}
	release, err := m.holdProfile()
	if err != nil {
		return HealthInfo{}, err
	}
	defer release()
	if m.profile.reopen != nil {
		cmd.Env = append(cmd.Env, "RSRS_DATA_DIR="+m.DataRoot)
	}
	log, err := os.CreateTemp(m.Root, "runtime-start-*.log")
	if err != nil {
		return HealthInfo{}, err
	}
	defer log.Close()
	cmd.Stdout, cmd.Stderr = log, log
	if err := cmd.Start(); err != nil {
		return HealthInfo{}, err
	}
	exited := make(chan error, 1)
	go func() { exited <- cmd.Wait() }()
	reaped := false
	defer func() {
		if resultErr != nil && !reaped {
			killErr := cmd.Process.Kill()
			waitErr := <-exited
			if cmd.ProcessState == nil {
				resultErr = errors.Join(resultErr, fmt.Errorf("Runtime serving child exit unconfirmed: %w", errors.Join(killErr, waitErr)))
			}
		}
	}()

	ticker := time.NewTicker(100 * time.Millisecond)
	defer ticker.Stop()
	for {
		h, _, err := m.Health(ctx)
		if err == nil {
			if h.PID != uint32(cmd.Process.Pid) {
				return HealthInfo{}, errors.New(i18n.Message7d43ff8e7e66eaf1)
			}
			if h.StartupComplete {
				if complete != nil {
					if err := complete(h); err != nil {
						return HealthInfo{}, err
					}
				}
				if err := ctx.Err(); err != nil {
					return HealthInfo{}, err
				}
				m.mu.Lock()
				m.directRuntime = h
				m.mu.Unlock()
				return h, nil
			}
		} else if !runtimeStartingProbeRetryable(err) && ctx.Err() == nil {
			return HealthInfo{}, err
		}
		select {
		case err := <-exited:
			reaped = true
			return HealthInfo{}, fmt.Errorf(i18n.Message4c44537e0ef9342b, err, log.Name())
		case <-ctx.Done():
			return HealthInfo{}, fmt.Errorf(i18n.Messagee777666a7c16500d, log.Name(), ctx.Err())
		case <-ticker.C:
		}
	}
}

func runtimeServingCommand(executable, installation, host, port, launchID string) *exec.Cmd {
	cmd := exec.Command(executable, "serve", "--host", host, "--port", port, "--data-dir", installation)
	cmd.Dir = filepath.Dir(executable)
	for _, variable := range os.Environ() {
		key, _, _ := strings.Cut(variable, "=")
		switch strings.ToUpper(key) {
		case "RSRS_RUNTIME_BOOT_TOKEN", "RSRS_SUPER", "RSRS_PASS", "RSRS_SECRET", "RSRS_SECRET_KEY",
			"ONEMEMORY_SUPER", "ONEMEMORY_PASS", "ONEMEMORY_SECRET", "ONEMEMORY_SECRET_KEY",
			"RESPIRE_SUPER", "RESPIRE_PASS", "RESPIRE_SECRET", "RESPIRE_SECRET_KEY":
			continue
		}
		cmd.Env = append(cmd.Env, variable)
	}
	if launchID != "" {
		cmd.Env = append(cmd.Env, "RSRS_RUNTIME_BOOT_TOKEN=launch-v1:"+launchID)
	}
	configureRuntimeCommand(cmd)
	return cmd
}
