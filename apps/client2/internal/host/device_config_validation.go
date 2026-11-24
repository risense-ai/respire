package host

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"respire-client2/internal/i18n"
	"runtime"
	"strconv"
	"strings"
	"time"
)

type deviceConfigReview struct {
	OriginalRevision  *string             `json:"original_revision"`
	CandidateRevision string              `json:"candidate_revision"`
	Intent            deviceConfigIntent  `json:"intent"`
	SourceProfile     string              `json:"source_profile"`
	TargetProfile     string              `json:"target_profile"`
	SourceIdentity    runtimeAuthIdentity `json:"source_identity"`
}

type deviceConfigValidation struct {
	deviceConfigReview
	SchemaVersion int                   `json:"schema_version"`
	Binding       runtimeControlBinding `json:"binding"`
	Identity      runtimeAuthIdentity   `json:"identity"`
	Original      *string               `json:"original_config"`
}

func (v deviceConfigValidation) request(id string) (runtimeControlRequest, error) {
	raw, err := json.Marshal(v)
	if err != nil {
		return runtimeControlRequest{}, err
	}
	defer clear(raw)
	return runtimeControlRequest{V: 1, ID: id, Method: "control.config_validate", Args: []string{string(raw)}}, nil
}

func deviceProfileIdentity(profile string) (runtimeAuthIdentity, error) {
	data, err := readCredentialConfig(filepath.Join(profile, "session.json"), controlFrameLimit)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return runtimeAuthIdentity{}, err
	}
	return runtimeAuthIdentityOf(data)
}

func (m *Manager) verifyDeviceConfigOwner(ctx context.Context, health HealthInfo, info InstalledRuntime, profile string) error {
	if err := m.verifySelectedHealth(health, info); err != nil {
		return err
	}
	birth, err := runtimeControlProcessBirth(ctx, int(health.PID))
	if err != nil {
		return err
	}
	prefix := map[string]string{"windows": "windows-filetime-", "linux": "linux-starttime-", "darwin": "darwin-lstart-"}[runtime.GOOS]
	if birth == "" || health.ProcessStartID != prefix+birth {
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
	identity, err := deviceProfileIdentity(profile)
	if err != nil {
		return err
	}
	account := controlDigest([]byte(profile))
	if identity.AccountID != nil {
		account = *identity.AccountID
	}
	if !health.StartupComplete || health.ImageSHA256 != info.SHA256 ||
		!samePath(health.ImagePath, health.Exe) || !samePath(health.DataDir, profile) ||
		health.InstallationID != controlDigest([]byte(root)) || health.AccountID != account ||
		!runtimeInstanceID.MatchString(health.InstanceID) || health.Generation == 0 {
		return errors.New(i18n.Message34a4deabb8b33b2c)
	}
	return nil
}

func (m *Manager) validateDeviceConfig(ctx context.Context, intent deviceConfigIntent) (json.RawMessage, error) {
	return m.changeDeviceConfig(ctx, intent, nil, configTargetDefault)
}

func (m *Manager) changeDeviceConfig(ctx context.Context, intent deviceConfigIntent, workspace *workspaceModeChange,
	targetKind deviceConfigTarget) (result json.RawMessage, err error) {
	takeover, err := m.lockRuntimeTakeover(ctx)
	if err != nil {
		return nil, err
	}
	defer func() { err = errors.Join(err, takeover.Close()) }()
	if err := m.requireNoDeviceMaintenance(); err != nil {
		return nil, err
	}
	var mode string
	if workspace != nil {
		mode = workspace.mode
	}
	edit, err := m.reviewDeviceConfig(intent, mode)
	if err != nil {
		return nil, err
	}
	root, err := canonicalSessionDirectory(m.DataRoot)
	if err != nil {
		return nil, err
	}
	directory := filepath.Join(root, "runtime", "autostart")
	if _, err := deviceProfilePath(directory); err != nil {
		return nil, err
	}
	health, _, err := m.Health(ctx)
	wasServing := err == nil
	if !wasServing && !IsUnavailable(err) {
		return nil, err
	}
	var info InstalledRuntime
	if workspace == nil || wasServing {
		info, err = m.Installed()
		if err != nil {
			return nil, err
		}
		if err := m.verifyInstalled(info); err != nil {
			return nil, err
		}
	}
	if wasServing {
		if err := m.verifyDeviceConfigOwner(ctx, health, info, edit.before.profile); err != nil {
			return nil, err
		}
		if health.ManagedID != "" {
			return nil, errors.New(i18n.Messagedde6cfe3b284325a)
		}
	}
	address, port, err := m.runtimeAccountEndpoint()
	if err != nil {
		return nil, err
	}
	var space *freshSpaceDirectory
	if targetKind == configTargetNewSpace {
		if err := checkRuntimeAccountDirectory(edit.before.profile); err != nil {
			return nil, fmt.Errorf(i18n.Message61335fa7f446c8eb, err)
		}
		space, err = createFreshSpaceDirectory(edit.after.profile)
		if err != nil {
			return nil, err
		}
		defer func() {
			var unknown *unconfirmedError
			if err != nil && !errors.As(err, &unknown) {
				err = errors.Join(err, space.removeEmpty())
			}
		}()
	} else if err := os.MkdirAll(edit.before.profile, 0700); err != nil {
		return nil, err
	}
	switch targetKind {
	case configTargetExisting:
		if err := profileDirectory(edit.after.profile); err != nil {
			return nil, fmt.Errorf(i18n.Messageedd99c36e1bdbae8, err)
		}
	case configTargetDefault:
		if err := os.MkdirAll(edit.after.profile, 0700); err != nil {
			return nil, err
		}
	}
	source, err := canonicalSessionDirectory(edit.before.profile)
	if err != nil || !samePath(source, edit.before.profile) {
		return nil, errors.Join(errors.New(i18n.Messageb2a792df1c2409cd), err)
	}
	target, err := canonicalSessionDirectory(edit.after.profile)
	if err != nil || !samePath(target, edit.after.profile) {
		return nil, errors.Join(errors.New(i18n.Messagedf040e3f0fe09b64), err)
	}
	sourceIdentity, err := deviceProfileIdentity(source)
	if err != nil {
		return nil, err
	}
	targetIdentity, err := deviceProfileIdentity(target)
	if err != nil {
		return nil, err
	}
	if space != nil && targetIdentity.Kind != "absent" {
		return nil, errors.New(i18n.Message20c9e41ec8bd847d)
	}
	if m.ClientVersion == "" {
		return nil, errors.New(i18n.Message5d05d15250a23f63)
	}
	birth, err := hostControlBirth(ctx)
	if err != nil {
		return nil, err
	}
	token, err := controlUUID()
	if err != nil {
		return nil, err
	}
	var params deviceConfigValidation
	var requestID string
	if workspace == nil {
		params = deviceConfigValidation{
			deviceConfigReview: deviceConfigReview{CandidateRevision: controlDigest(edit.candidate), Intent: intent,
				SourceProfile: source, TargetProfile: target, SourceIdentity: sourceIdentity},
			SchemaVersion: 1, Identity: targetIdentity,
			Binding: runtimeControlBinding{root, target, token, targetIdentity.AccountID},
		}
		if edit.original != nil {
			value, digest := string(edit.original), controlDigest(edit.original)
			params.Original, params.OriginalRevision = &value, &digest
		}
		requestID, err = controlUUID()
		if err != nil {
			return nil, err
		}
		// The real launch grant substitutes a UUID of the same encoded width.
		preview, err := params.request(requestID)
		if err != nil {
			return nil, err
		}
		frame, err := json.Marshal(preview)
		if err != nil {
			return nil, err
		}
		size := len(frame)
		clear(frame)
		if size > controlFrameLimit {
			return nil, errors.New(i18n.Message2284d79bfad32f54)
		}
	}
	if err := os.MkdirAll(directory, 0700); err != nil {
		return nil, err
	}
	marker := runtimeMaintenanceMarker{Schema: 1, Token: token, ExpectedVersion: m.ClientVersion,
		OwnerPID: os.Getpid(), OwnerBirth: birth}
	markerPath := filepath.Join(directory, "maintenance.json")
	var maintenance runtimeAccountAdmission
	updateMarker := func(phase, profile string) error {
		marker.Phase, marker.ExpectedProfile = phase, profile
		next, err := json.Marshal(marker)
		if err != nil {
			return err
		}
		if err := writeRuntimeAccountRecord(markerPath, next, maintenance.Marker); err != nil {
			return err
		}
		maintenance.Marker = next
		return nil
	}
	image := filepath.Join(m.runtimeDir(info.Version), runtimeBinary())
	staged, starting, publishingSpace := false, false, false
	var launchID string
	var launchedPID uint32
	defer func() {
		if err == nil || maintenance.Marker == nil {
			return
		}
		if publishingSpace {
			err = &unconfirmedError{cause: errors.Join(err, errors.New(i18n.Message59bbeb7408216940))}
			return
		}
		if workspace != nil && !wasServing && !staged {
			if cleanupErr := removeOwnedRuntimeAccountRecord(markerPath, maintenance.Marker); cleanupErr != nil {
				err = &unconfirmedError{cause: errors.Join(err, cleanupErr)}
			}
			return
		}
		cleanup, cancel := context.WithTimeout(context.WithoutCancel(ctx), 35*time.Second)
		defer cancel()
		if starting {
			active, _, probeErr := m.Health(cleanup)
			if probeErr == nil && (active.InstanceID != launchID || active.PID != launchedPID) {
				probeErr = errors.New(i18n.Message49ba9059ac58bb5a)
			}
			if probeErr == nil {
				probeErr = m.verifyDeviceConfigOwner(cleanup, active, info, target)
			}
			if probeErr == nil {
				probeErr = m.stopDetached(cleanup, active)
			}
			if probeErr != nil {
				err = &unconfirmedError{cause: errors.Join(err, errors.New(i18n.Messagef3cb802643d72009),
					probeErr, updateMarker("Paused", target))}
				return
			}
		}
		if strings.Contains(err.Error(), "exit unconfirmed") {
			err = &unconfirmedError{cause: errors.Join(err, errors.New(i18n.Message11bf3ad9930e7a5d),
				updateMarker("Paused", target))}
			return
		}
		if pauseErr := updateMarker("Paused", source); pauseErr != nil {
			err = &unconfirmedError{cause: errors.Join(err, pauseErr)}
			return
		}
		if staged {
			if restoreErr := edit.restore(cleanup); restoreErr != nil {
				err = &unconfirmedError{cause: errors.Join(err, restoreErr)}
				return
			}
		}
		err = fmt.Errorf(i18n.Message7a2e8a47e5d8293b, err)
		if space != nil {
			err = &unconfirmedError{cause: err}
		}
	}()

	var teamReadonly bool
	if workspace != nil {
		teamReadonly, err = workspace.check(root, target)
		if err != nil {
			return nil, err
		}
	}
	if wasServing {
		if err := m.stopDetached(ctx, health); err != nil {
			return nil, err
		}
	}
	if err := m.admitDeviceConfig(ctx, root, source, target, address, port, workspace == nil || wasServing, func(coordinator string) error {
		marker.ID = coordinator
		if space != nil {
			if err := space.verifyIdentity(); err != nil {
				return err
			}
		}
		if workspace != nil {
			teamReadonly, err = workspace.check(root, target)
			if err != nil {
				return err
			}
			identity, err := deviceProfileIdentity(target)
			if err != nil || !reflect.DeepEqual(identity, targetIdentity) {
				return errors.Join(errors.New(i18n.Messagea27993818a50b868), err)
			}
		}
		if err := updateMarker("Paused", target); err != nil {
			return err
		}
		if workspace != nil {
			staged, err = edit.publish(ctx)
			return err
		}
		return nil
	}); err != nil {
		return nil, err
	}
	if space != nil {
		// SQLite may have temporary sidecars while admission holds the target lock.
		if err := space.verifyEmpty(); err != nil {
			return nil, err
		}
	}
	var off bool
	if workspace == nil {
		control, err := m.prepareRuntimeAccountControl(ctx, image, info.SHA256, target, targetIdentity.AccountID, maintenance.Marker)
		if err != nil {
			return nil, err
		}
		params.Binding = control.binding
		request, err := params.request(requestID)
		if err != nil {
			return nil, err
		}
		var publishErr error
		if staged, publishErr = edit.publish(ctx); publishErr != nil {
			return nil, publishErr
		}
		validation, cancel := context.WithTimeout(ctx, 10*time.Second)
		reply, err := m.exchangeRuntimeControl(validation, control.image, control.port, control.grant, request)
		cancel()
		if err != nil {
			return nil, err
		}
		off, err = verifyDeviceConfigReceipt(reply, params.deviceConfigReview, targetIdentity)
		if err != nil {
			return nil, err
		}
	} else {
		off = workspace.mode == "off"
	}
	if err := edit.verifyCandidate(source, target, sourceIdentity, targetIdentity); err != nil {
		return nil, err
	}

	applied := false
	idle := space != nil || targetIdentity.Kind == "unbound" ||
		(targetIdentity.Kind == "absent" && !wasServing && source == target && sourceIdentity.Kind == "absent")
	if workspace != nil {
		idle = !wasServing
	}
	storeReady := false
	if !idle {
		// A private validation receipt cannot confirm a serving Runtime.
		record := runtimeAccountRecord{Root: root, Source: source, Token: token}
		launch, err := m.prepareRuntimeProfileLaunch(ctx, image, info.SHA256, record, &maintenance, target, targetIdentity.AccountID)
		if err != nil {
			return nil, err
		}
		cmd := runtimeServingCommand(launch.image, m.DataRoot, "127.0.0.1", strconv.FormatUint(launch.port, 10), launch.binding.LaunchID)
		launchID = launch.binding.LaunchID
		startup, cancel := context.WithTimeout(ctx, 90*time.Second)
		active, launchErr := m.launchRuntimeServing(startup, cmd)
		cancel()
		starting = cmd.Process != nil
		if starting {
			launchedPID = uint32(cmd.Process.Pid)
		}
		if launchErr != nil {
			return nil, launchErr
		}
		if err := m.verifyDeviceConfigOwner(ctx, active, info, target); err != nil {
			return nil, err
		}
		if active.InstanceID != launch.binding.LaunchID {
			return nil, errors.New(i18n.Messagecac1928247ade13f)
		}
		if !active.StoreReady && !off && targetIdentity.AccountID != nil {
			unlock, cancel := context.WithTimeout(ctx, 30*time.Second)
			active, err = m.unlockRuntimeProfileTarget(unlock, launch, maintenance, record, target,
				*targetIdentity.AccountID, targetIdentity.SessionDigest, active.PID)
			cancel()
			if err != nil {
				return nil, err
			}
		}
		allowLocked := workspace != nil && targetIdentity.Kind != "bound" && !health.StoreReady
		if !active.StoreReady && !off && !allowLocked {
			return nil, errors.New(i18n.Message7c4e052d94b4dfac)
		}
		if workspace != nil && off && active.StoreReady {
			return nil, errors.New(i18n.Message49cd2691c472281e)
		}
		storeReady = active.StoreReady
		if wasServing {
			applied = true
		} else {
			if err := m.stopDetached(ctx, active); err != nil {
				return nil, err
			}
			starting = false
		}
	}
	if workspace == nil || wasServing {
		current, err := m.Installed()
		if err != nil || current != info {
			return nil, errors.Join(errors.New(i18n.Message22ae1f5488eb20c7), err)
		}
		if err := m.verifyInstalled(info); err != nil {
			return nil, err
		}
	}
	if !applied {
		if err := requireClosedRuntimePort(ctx, address); err != nil {
			return nil, err
		}
	}
	configLock, err := lockDeviceConfig(ctx, edit.path)
	if err != nil {
		return nil, err
	}
	defer configLock.Close()
	if err := edit.verifyCandidate(source, target, sourceIdentity, targetIdentity); err != nil {
		return nil, err
	}
	if space != nil {
		if err := space.verifyEmpty(); err != nil {
			return nil, err
		}
		result, err = space.result()
	} else if workspace == nil {
		result, err = m.deviceConfigResult(ctx, edit.after, applied)
	} else {
		if applied {
			active, _, err := m.Health(ctx)
			if err != nil {
				return nil, err
			}
			if err := m.verifyDeviceConfigOwner(ctx, active, info, target); err != nil {
				return nil, err
			}
			if active.InstanceID != launchID || active.StoreReady != storeReady {
				return nil, errors.New(i18n.Message280bcfc371c18521)
			}
		}
		teamReadonly, err = workspace.check(root, target)
		if err != nil {
			return nil, err
		}
		result, err = workspace.result(teamReadonly, applied, storeReady)
	}
	if err != nil {
		return nil, err
	}
	if err := updateMarker("Verified", target); err != nil {
		return nil, err
	}
	if space != nil {
		publishingSpace = true
		if err := space.publishOwner(); err != nil {
			return nil, err
		}
	}
	if err := removeOwnedRuntimeAccountRecord(markerPath, maintenance.Marker); err != nil {
		return nil, err
	}
	return result, nil
}

// The current listener and these locks decide whether historical records can
// be archived. Runtime rechecks the Paused grant after taking its own locks.
func (m *Manager) withRuntimeLibraryLocks(ctx context.Context, root, source, target string, run func() error) (err error) {
	for i, profile := range []string{root, source, target} {
		if (i > 0 && samePath(profile, root)) || (i == 2 && samePath(target, source)) {
			continue
		}
		lock, lockErr := m.lockRuntimeDirectory(ctx, profile)
		if lockErr != nil {
			return fmt.Errorf(i18n.Message2023f835f00835c7, profile, lockErr)
		}
		defer func() { err = errors.Join(err, lock.Close()) }()
	}
	return run()
}

func (m *Manager) admitDeviceConfig(ctx context.Context, root, source, target, address string, port uint64, archiveRecords bool, pause func(string) error) error {
	return m.withRuntimeLibraryLocks(ctx, root, source, target, func() error {
		return m.admitLockedDeviceConfig(ctx, root, address, port, archiveRecords, pause)
	})
}
