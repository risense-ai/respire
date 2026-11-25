package host

import "respire-client2/internal/i18n"

import (
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"strings"
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
