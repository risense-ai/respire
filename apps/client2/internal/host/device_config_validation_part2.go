package host

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"reflect"
	"respire-client2/internal/i18n"
	"runtime"
	"strconv"
	"strings"
	"unicode/utf8"
)

func (m *Manager) admitLockedDeviceConfig(ctx context.Context, root, address string, port uint64, archiveRecords bool, pause func(string) error) error {
	if err := requireClosedRuntimePort(ctx, address); err != nil {
		return err
	}
	if err := m.requireNoDeviceMaintenance(); err != nil {
		return err
	}
	if !archiveRecords {
		coordinator, err := runtimeRegistrationCoordinator(root, nil)
		if err != nil {
			return err
		}
		return pause(coordinator)
	}
	coordinator, err := deviceConfigCoordinator(root, port)
	if err != nil {
		return err
	}
	endpoint := filepath.Join(root, "runtime", "endpoint.json")
	if info, err := os.Lstat(endpoint); err == nil {
		if err := archiveRuntimeRecord(endpoint, info); err != nil {
			return err
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	return pause(coordinator)
}

func deviceConfigCoordinator(root string, port uint64) (string, error) {
	path := filepath.Join(root, "runtime", "autostart", "registration.json")
	info, err := os.Lstat(path)
	if errors.Is(err, os.ErrNotExist) {
		return runtimeRegistrationCoordinator(root, nil)
	}
	if err != nil {
		return "", err
	}
	if info.Mode().IsRegular() && info.Size() <= controlFrameLimit {
		bytes, err := readCredentialConfig(path, controlFrameLimit)
		if err != nil {
			return "", err
		}
		if id := deviceConfigRegistrationID(bytes, root, port); id != "" {
			return id, nil
		}
	}
	if err := archiveRuntimeRecord(path, info); err != nil {
		return "", err
	}
	return runtimeRegistrationCoordinator(root, nil)
}

// dev.28 checks this tuple even for private, non-supervised maintenance.
func deviceConfigRegistrationID(raw []byte, root string, port uint64) string {
	var registration map[string]json.RawMessage
	if !utf8.Valid(raw) || json.Unmarshal(raw, &registration) != nil || string(registration["schema"]) != "1" {
		return ""
	}
	readString := func(fields map[string]json.RawMessage, key string) string {
		var value string
		if json.Unmarshal(fields[key], &value) != nil {
			return ""
		}
		return value
	}
	owner := readString(registration, "owner")
	installation := readString(registration, "installation")
	if owner == "" || filepath.Clean(installation) != filepath.Clean(root) {
		return ""
	}
	for _, component := range strings.Split(filepath.ToSlash(installation), "/") {
		if component == ".." {
			return ""
		}
	}
	platform := runtime.GOOS
	if platform == "darwin" {
		platform = "macos"
	}
	id := controlDigest([]byte("respire-runtime-v1\x00" + root + "\x00" + owner))[:24]
	if readString(registration, "platform") != platform || readString(registration, "id") != id {
		return ""
	}
	var environment map[string]json.RawMessage
	if json.Unmarshal(registration["environment"], &environment) != nil {
		return ""
	}
	rawPort, rawRoot := readString(environment, "RSRS_RPC_PORT"), readString(environment, "RSRS_DATA_DIR")
	registeredPort, err := strconv.ParseUint(strings.TrimPrefix(rawPort, "+"), 10, 16)
	if err != nil || registeredPort != port || rawRoot == "" {
		return ""
	}
	registeredRoot, err := canonicalSessionDirectory(rawRoot)
	if err != nil || registeredRoot != root {
		return ""
	}
	return id
}

func (e deviceConfigEdit) verifyCandidate(source, target string, sourceIdentity, targetIdentity runtimeAuthIdentity) error {
	current, err := deviceConfigBytes(e.path)
	if err != nil {
		return err
	}
	if !bytes.Equal(current, e.candidate) {
		return errors.New(i18n.Messagee7370feef0e4dac2)
	}
	for _, entry := range []struct {
		profile  string
		identity runtimeAuthIdentity
	}{{source, sourceIdentity}, {target, targetIdentity}} {
		current, err := canonicalSessionDirectory(entry.profile)
		if err != nil || current != entry.profile {
			return errors.Join(errors.New(i18n.Messageb0f16d546c8ca8f1), err)
		}
		identity, err := deviceProfileIdentity(current)
		if err != nil || !reflect.DeepEqual(identity, entry.identity) {
			return errors.Join(errors.New(i18n.Message74b10ec7271cac3b), err)
		}
	}
	return nil
}

func verifyDeviceConfigReceipt(reply rpcResponse, review deviceConfigReview, target runtimeAuthIdentity) (bool, error) {
	var envelope struct {
		Command string
		Status  string
		Summary json.RawMessage
		Details json.RawMessage
		Errors  []string
		Items   []struct{ Name, Status, Value string }
	}
	if json.Unmarshal(reply.Envelope, &envelope) != nil || envelope.Command != "config-validate" ||
		envelope.Status != "ok" || string(envelope.Details) != "null" || len(envelope.Errors) != 0 {
		return false, errors.New(i18n.Message9d02b123a12a54ce)
	}
	if reply.DataDir != "" && !samePath(reply.DataDir, review.TargetProfile) {
		return false, errors.New(i18n.Message032e1c94aa0a30f7)
	}
	ready, stopped := 0, 0
	for _, item := range envelope.Items {
		switch item.Name {
		case "control_ready":
			if item.Status != "ok" || item.Value != "true" {
				return false, errors.New(i18n.Messagebf5b14c176917641)
			}
			ready++
		case "store_ready":
			if item.Status != "skip" || item.Value != "false" {
				return false, errors.New(i18n.Message3c280deb29051d23)
			}
			stopped++
		}
	}
	var receipt struct {
		deviceConfigReview
		TargetIdentity runtimeAuthIdentity `json:"target_identity"`
		OffMode        *bool               `json:"off_mode"`
	}
	decoder := json.NewDecoder(bytes.NewReader(envelope.Summary))
	decoder.DisallowUnknownFields()
	if ready != 1 || stopped != 1 || decoder.Decode(&receipt) != nil || receipt.OffMode == nil ||
		!reflect.DeepEqual(receipt.deviceConfigReview, review) || !reflect.DeepEqual(receipt.TargetIdentity, target) {
		return false, errors.New(i18n.Messaged9f279805d477337)
	}
	return *receipt.OffMode, nil
}
