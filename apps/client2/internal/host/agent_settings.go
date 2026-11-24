package host

import "respire-client2/internal/i18n"

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
)

func readAgentSettingsObject(path string) (map[string]any, error) {
	raw, err := readCredentialConfig(path, 1<<20)
	if errors.Is(err, os.ErrNotExist) {
		return map[string]any{}, nil
	}
	if err != nil {
		return nil, err
	}
	var settings map[string]any
	if err := json.Unmarshal(raw, &settings); err != nil {
		return nil, fmt.Errorf("%s: %w", filepath.Base(path), err)
	}
	if settings == nil {
		return nil, fmt.Errorf(i18n.Message4a5506e86a275256, filepath.Base(path))
	}
	return settings, nil
}

func (m *Manager) settingsWithWorkspaceMode() (map[string]any, string, error) {
	_, device, err := m.readDeviceConfig()
	if err != nil {
		return nil, "", err
	}
	settings, err := readAgentSettingsObject(filepath.Join(device.profile, "agent.json"))
	if err != nil {
		return nil, "", err
	}

	mode := "normal"
	for _, name := range []string{"readonly_team", "readonly", "memory_off"} {
		if value, present := settings[name]; present {
			if _, ok := value.(bool); !ok {
				return nil, "", fmt.Errorf(i18n.Messagedbee957669291a03, name)
			}
		}
	}
	if value, present := device.fields["service_mode"]; present {
		if err := json.Unmarshal(value, &mode); err != nil {
			return nil, "", err
		}
	} else if settings["memory_off"] == true {
		mode = "off"
	} else if settings["readonly"] == true {
		mode = "readonly"
	}
	if mode == "normal" && settings["readonly_team"] == true {
		mode = "readonly"
	}
	return settings, mode, nil
}

func (m *Manager) WorkspaceMode() (string, error) {
	_, mode, err := m.settingsWithWorkspaceMode()
	return mode, err
}

func (m *Manager) desktopAgentSettings() (json.RawMessage, error) {
	settings, mode, err := m.settingsWithWorkspaceMode()
	if err != nil {
		return nil, err
	}
	result := map[string]any{"workspace_mode": mode}
	for _, name := range []string{"diary_mode", "recall_mode", "recall_api_base", "recall_model"} {
		if value, present := settings[name]; present {
			if _, ok := value.(string); !ok {
				return nil, fmt.Errorf(i18n.Message464f0e7be9c1332c, name)
			}
			result[name] = value
		}
	}
	configured := map[string]any{}
	if value, present := settings["recall_fusion"]; present {
		var ok bool
		configured, ok = value.(map[string]any)
		if !ok {
			return nil, errors.New(i18n.Messagee688b52d90562bba)
		}
	}
	result["recall_fusion_runtime"], err = desktopRecallFusion(configured)
	if err != nil {
		return nil, err
	}
	return json.Marshal(map[string]any{"command": "agent-config", "status": "ok", "summary": result})
}

func desktopRecallFusion(configured map[string]any) (map[string]any, error) {
	defaults := map[string]any{"fusion": "legacy", "fusion_alpha": 0.20, "fusion_beta": 0.25,
		"fusion_temperature": 1.0, "fusion_gate_recency": 0.02, "fusion_gate_recency_temporal": 1.0,
		"fusion_gate_hot": 0.10, "fusion_gate_tree": 0.02, "fusion_temporal_alpha": 0.15}
	effective := make(map[string]any, len(defaults))
	for name, value := range defaults {
		effective[name] = value
	}
	for name, value := range configured {
		if _, ok := defaults[name]; !ok {
			return nil, fmt.Errorf(i18n.Message5797930d3be127c3, name)
		}
		effective[name] = value
	}
	if err := validateDesktopRecallFusion(effective); err != nil {
		return nil, err
	}
	overrides := []string{}
	profile, hasProfile := os.LookupEnv("RSRS_RECALL_PROFILE")
	if hasProfile && profile != "legacy" && profile != "calibrated" {
		return nil, errors.New(i18n.Messagea4a8c0765906662c)
	}
	if profile == "legacy" {
		effective["fusion"] = "legacy"
	}
	for name := range defaults {
		raw, present := os.LookupEnv("RSRS_RECALL_" + strings.ToUpper(name))
		if !present {
			if name == "fusion" && profile == "legacy" {
				overrides = append(overrides, name)
			}
			continue
		}
		if name == "fusion" {
			effective[name] = strings.TrimSpace(raw)
		} else {
			number, err := strconv.ParseFloat(strings.TrimSpace(raw), 32)
			if err != nil {
				return nil, fmt.Errorf(i18n.Message0c9d56e1e19c7f5b, name)
			}
			effective[name] = number
		}
		overrides = append(overrides, name)
	}
	if err := validateDesktopRecallFusion(effective); err != nil {
		return nil, err
	}
	sort.Strings(overrides)
	return map[string]any{"scope": "host_configuration", "runtime_verified": false, "configured": configured,
		"effective": effective, "defaults": defaults, "overrides": overrides, "applies": "next_query"}, nil
}

func validateDesktopRecallFusion(settings map[string]any) error {
	for name, raw := range settings {
		if name == "fusion" {
			switch raw {
			case "legacy", "attention", "attention_anchor", "auto":
				continue
			default:
				return errors.New(i18n.Message2e05ddb586b2bccf)
			}
		}
		value, ok := raw.(float64)
		if !ok || !(value >= 0 && (name == "fusion_temperature" && float32(value) > 0 && value <= 10 ||
			name != "fusion_temperature" && value <= 1)) {
			return fmt.Errorf(i18n.Message112fe0af7a1e4ef2, name)
		}
	}
	if float32(settings["fusion_alpha"].(float64))+float32(settings["fusion_beta"].(float64)) > 1 {
		return errors.New(i18n.Messageac97d30d6cc8d65d)
	}
	return nil
}
