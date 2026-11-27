package host

import (
	"context"
	"encoding/json"
	"errors"
	"net/http"
	"os"
	"path/filepath"
	"strings"

	"respire-client2/internal/i18n"

	runtimesdk "github.com/risense-ai/respire/packages/runtime-sdk-go"
)

func isDesktopAgentTextSetting(key string) bool {
	switch key {
	case "diary_mode", "recall_mode", "recall_api_base", "recall_model":
		return true
	default:
		return false
	}
}

// Runtime reads these settings from the selected profile for each operation.
func (m *Manager) setDesktopAgentTextSetting(ctx context.Context, id, setting string) (json.RawMessage, error) {
	key, value, hasValue := strings.Cut(setting, "=")
	key = strings.TrimSpace(key)
	if !hasValue || !isDesktopAgentTextSetting(key) {
		return nil, errors.New(i18n.Message79a4e914dccc6982)
	}
	if key == "diary_mode" && value != "concise" && value != "verbose" {
		return nil, errors.New(i18n.Message350c9311dce5e812)
	}
	if key == "recall_mode" && value != "fast" && value != "quality" {
		return nil, errors.New(i18n.Message48d0a793bafa582b)
	}
	encoded, err := json.Marshal(value)
	if err != nil {
		return nil, err
	}
	if key == "diary_mode" {
		release, err := m.holdProfile()
		if err != nil {
			return nil, err
		}
		defer release()
		client, supported, err := m.runtimeSDK(ctx, "openapi.v1")
		if err != nil && !IsUnavailable(err) {
			return nil, err
		}
		if supported {
			if err := m.markV1Operation(id, ""); err != nil {
				return nil, err
			}
			raw, err := sdkJSON(client.PatchHostConfig(ctx, runtimesdk.PatchHostConfigRequest{DiaryMode: &value}, m.authorizeRuntime))
			if err != nil {
				var rejected *RuntimeHTTPError
				if errors.As(err, &rejected) && rejected.Status < http.StatusInternalServerError &&
					rejected.Status != http.StatusRequestTimeout && !strings.HasSuffix(rejected.Code, "_outcome_unknown") {
					return nil, err
				}
				return nil, &unconfirmedError{id, err}
			}
			var config runtimesdk.HostConfig
			if err := json.Unmarshal(raw, &config); err != nil || config.DiaryMode != value {
				return nil, &unconfirmedError{id, errors.New("Runtime did not confirm the requested diary mode")}
			}
			return desktopAgentSettingResult(key, encoded)
		}
	}
	takeover, err := m.lockRuntimeTakeover(ctx)
	if err != nil {
		return nil, err
	}
	defer takeover.Close()
	profile, err := m.coreConfigProfile()
	if err != nil {
		return nil, err
	}
	if _, err := os.Lstat(filepath.Join(m.DataRoot, "runtime", "autostart", "maintenance.json")); err == nil {
		return nil, errors.New(i18n.Messagef013229a0e2b4af3)
	} else if !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	settings, before, err := readCoreConfigFile(profile)
	if err != nil {
		return nil, err
	}
	settings[key] = encoded
	change := &coreConfigChange{Profile: profile, Before: before, Key: key}
	if err := m.publishDesktopAgentSettings(id, change, settings); err != nil {
		return nil, err
	}
	return desktopAgentSettingResult(key, encoded)
}

func desktopAgentSettingResult(key string, value json.RawMessage) (json.RawMessage, error) {
	return json.Marshal(map[string]any{"command": "agent-config", "status": "ok", "summary": map[string]any{
		"action": "updated", "key": key, "value": value,
	}})
}
