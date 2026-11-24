package host

import "respire-client2/internal/i18n"

import (
	"context"
	"crypto/rand"
	"crypto/sha256"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"reflect"
	"strings"
)

var errCoreConfigRejected = errors.New(i18n.Messagec173e55ccdf62dfe)

type coreConfigCandidate struct {
	ReadHeat     any `json:"read_heat"`
	RecallFusion any `json:"recall_fusion"`
}

type coreConfigChange struct {
	Profile     string              `json:"profile"`
	Scope       runtimeScope        `json:"scope"`
	Before      string              `json:"before"`
	OfflineFile string              `json:"offline_file,omitempty"`
	Revision    uint64              `json:"revision"`
	Key         string              `json:"key"`
	Target      coreConfigCandidate `json:"target"`
}

type coreConfigResult struct {
	State     string              `json:"state"`
	Revision  *uint64             `json:"revision"`
	Candidate coreConfigCandidate `json:"candidate"`
	Effective struct {
		ReadHeat     *bool          `json:"read_heat"`
		RecallFusion map[string]any `json:"recall_fusion"`
	} `json:"effective"`
	Capabilities struct {
		ReadHeat     int `json:"read_heat_contract"`
		RecallFusion int `json:"recall_fusion_contract"`
	} `json:"capabilities"`
	Applies string `json:"applies"`
}

func readCoreConfigFile(profile string) (map[string]json.RawMessage, string, error) {
	path := filepath.Join(profile, "agent.json")
	data, err := readCredentialConfig(path, 1<<20)
	if errors.Is(err, os.ErrNotExist) {
		return map[string]json.RawMessage{}, "", nil
	}
	if err != nil {
		return nil, "", err
	}
	var settings map[string]json.RawMessage
	if err := json.Unmarshal(data, &settings); err != nil {
		return nil, "", err
	}
	if settings == nil {
		return nil, "", errors.New(i18n.Message5f815772bbe7b34a)
	}
	return settings, fmt.Sprintf("%x", sha256.Sum256(data)), nil
}

func (c coreConfigChange) value() any {
	if c.Key == "read_heat" {
		return c.Target.ReadHeat
	}
	return c.Target.RecallFusion
}

func (m *Manager) coreConfigProfile() (string, error) {
	device, err := readAgentSettingsObject(filepath.Join(m.DataRoot, "client.json"))
	if err != nil {
		return "", err
	}
	path, err := desktopProfilePath(m.DataRoot, device)
	if err != nil {
		return "", err
	}
	return filepath.EvalSymlinks(path)
}

func (m *Manager) coreConfigScope(ctx context.Context, profile string) (runtimeScope, error) {
	h, _, err := m.Health(ctx)
	if err != nil {
		return runtimeScope{}, err
	}
	if !samePath(h.InstallationRoot, m.DataRoot) || !samePath(h.DataDir, profile) ||
		!validHash(h.AccountID) || !runtimeInstanceID.MatchString(h.InstanceID) || h.Generation == 0 {
		return runtimeScope{}, errors.New(i18n.Messageffeaa3c14e08ff8b)
	}
	return runtimeScope{h.AccountID, h.InstanceID, h.Generation}, nil
}

func (m *Manager) requestCoreConfig(ctx context.Context, id, method string, scope runtimeScope, params map[string]any) (json.RawMessage, error) {
	params["schema_version"] = 1
	payload, err := json.Marshal(params)
	if err != nil {
		return nil, err
	}
	reply, err := m.rpcResponse(ctx, method, []string{string(payload)}, id, &scope)
	if err != nil {
		return nil, err
	}
	if reply.Scope == nil || *reply.Scope != scope || reply.OperationID != id ||
		reply.Exit < 0 || (reply.Exit == 0) != reply.OK {
		return nil, errors.New(i18n.Messagec0dee628263ce2a7)
	}
	err = responseError(reply)
	if !reply.OK && reply.Code == "core_config_rejected" && err != nil {
		return nil, fmt.Errorf("%w: %w", errCoreConfigRejected, err)
	}
	return reply.Envelope, err
}

func configResult(raw json.RawMessage, state string) (coreConfigResult, error) {
	var response struct {
		Summary coreConfigResult `json:"summary"`
	}
	if err := json.Unmarshal(raw, &response); err != nil {
		return response.Summary, err
	}
	result := response.Summary
	if result.State != state || result.Revision == nil || result.Applies != "next_query" ||
		result.Capabilities.ReadHeat != 1 || result.Capabilities.RecallFusion != 1 ||
		result.Effective.ReadHeat == nil || result.Effective.RecallFusion == nil {
		return result, errors.New(i18n.Message563ad79d45d3acf0)
	}
	return result, nil
}

func (m *Manager) setDesktopCoreConfig(ctx context.Context, id, setting string) (json.RawMessage, error) {
	key, value, _ := strings.Cut(setting, "=")
	key = strings.TrimSpace(key)
	var parsed any
	if err := json.Unmarshal([]byte(value), &parsed); err != nil {
		return nil, fmt.Errorf(i18n.Message898fafa9fc08e8c0, err)
	}
	switch key {
	case "read_heat":
		if _, ok := parsed.(bool); !ok {
			return nil, errors.New(i18n.Message433f95dbb5e9e94c)
		}
	case "recall_fusion":
		if _, ok := parsed.(map[string]any); !ok {
			return nil, errors.New(i18n.Messagee688b52d90562bba)
		}
	default:
		return nil, errors.New(i18n.Message23fcd18208ea8c63)
	}
	lock, err := m.lockRuntimeTakeover(ctx)
	if err != nil {
		return nil, err
	}
	defer lock.Close()
	profile, err := m.coreConfigProfile()
	if err != nil {
		return nil, err
	}
	settings, digest, err := readCoreConfigFile(profile)
	if err != nil {
		return nil, err
	}
	settings[key] = json.RawMessage(value)
	candidateJSON, err := json.Marshal(map[string]json.RawMessage{"read_heat": settings["read_heat"], "recall_fusion": settings["recall_fusion"]})
	if err != nil {
		return nil, err
	}
	var target coreConfigCandidate
	if err := json.Unmarshal(candidateJSON, &target); err != nil {
		return nil, err
	}
	change := &coreConfigChange{Profile: profile, Before: digest, Key: key, Target: target}
	scope, err := m.coreConfigScope(ctx, profile)
	if IsUnavailable(err) {
		return m.saveOfflineCoreConfig(ctx, id, change, settings)
	}
	if err != nil {
		return nil, err
	}
	change.Scope = scope
	raw, err := m.requestCoreConfig(ctx, id+"-validate", "core_config.validate", scope, map[string]any{"candidate": target})
	if err != nil {
		return nil, err
	}
	validated, err := configResult(raw, "validated")
	if err != nil {
		return nil, err
	}
	if !reflect.DeepEqual(validated.Candidate, target) {
		return nil, errors.New(i18n.Message057522282364a57f)
	}
	_, currentDigest, err := readCoreConfigFile(profile)
	if err != nil {
		return nil, err
	}
	if currentDigest != digest {
		return nil, errors.New(i18n.Message6692305fe747e002)
	}
	change.Revision = *validated.Revision
	if err := m.recordCoreConfigChange(id, change); err != nil {
		return nil, err
	}
	raw, err = m.requestCoreConfig(ctx, id, "core_config.apply", scope, map[string]any{"candidate": target, "expected_revision": change.Revision})
	if err != nil {
		if errors.Is(err, errCoreConfigRejected) {
			return nil, err
		}
		return nil, &unconfirmedError{id, err}
	}
	return m.finishDesktopCoreConfig(ctx, id, change, raw)
}

func (m *Manager) recordCoreConfigChange(id string, change *coreConfigChange) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	job := m.jobs[id]
	job.coreChange = change
	previous := job.Transaction
	if isDesktopAgentTextSetting(change.Key) {
		job.Transaction = &hostTransaction{OperationID: strings.TrimPrefix(id, "desktop-"), SourceProfile: change.Profile, Kind: "agent_setting"}
	}
	err := m.persistOperationsLocked()
	if err != nil {
		job.coreChange = nil
		job.Transaction = previous
	}
	return err
}

func (m *Manager) recoverDesktopCoreConfig(ctx context.Context, job Operation) (json.RawMessage, error) {
	change := job.coreChange
	if change == nil {
		return nil, errors.New(i18n.Message132fbf3b79192997)
	}
	hostSetting := isDesktopAgentTextSetting(change.Key)
	if (!hostSetting && change.Key != "read_heat" && change.Key != "recall_fusion") || hostSetting && change.OfflineFile == "" {
		return nil, &unconfirmedError{job.ID, errors.New(i18n.Message29e5db121268243d)}
	}
	lock, err := m.lockRuntimeTakeover(ctx)
	if err != nil {
		return nil, &unconfirmedError{job.ID, err}
	}
	defer lock.Close()
	if change.OfflineFile != "" {
		profile, err := m.coreConfigProfile()
		if err != nil || !samePath(profile, change.Profile) {
			return nil, &unconfirmedError{job.ID, errors.Join(errors.New(i18n.Message86471cbddd91adba), err)}
		}
		settings, actual, err := readCoreConfigFile(profile)
		if err != nil || actual != change.OfflineFile {
			return nil, &unconfirmedError{job.ID, errors.Join(errors.New(i18n.Messagedd1fa699ac17ea64), err)}
		}
		if hostSetting {
			return desktopAgentSettingResult(change.Key, settings[change.Key])
		}
		return offlineCoreConfigResult(change)
	}
	raw, err := m.inspectResult(ctx, job.ID, &change.Scope, errors.New(i18n.Message79e2a0055458e043))
	if err != nil {
		return nil, err
	}
	return m.finishDesktopCoreConfig(ctx, job.ID, change, raw)
}

// An offline save is a persisted setting for the next Runtime startup.
func (m *Manager) saveOfflineCoreConfig(ctx context.Context, id string, change *coreConfigChange, settings map[string]json.RawMessage) (json.RawMessage, error) {
	if change.Target.ReadHeat != nil {
		if _, ok := change.Target.ReadHeat.(bool); !ok {
			return nil, errors.New(i18n.Message433f95dbb5e9e94c)
		}
	}
	if change.Target.RecallFusion != nil {
		fusion, ok := change.Target.RecallFusion.(map[string]any)
		if !ok {
			return nil, errors.New(i18n.Messagee688b52d90562bba)
		}
		if _, err := desktopRecallFusion(fusion); err != nil {
			return nil, err
		}
	}
	rootLock, err := m.lockRuntimeDirectory(ctx, m.DataRoot)
	if err != nil {
		return nil, err
	}
	defer rootLock.Close()
	if !samePath(change.Profile, m.DataRoot) {
		profileLock, err := m.lockRuntimeDirectory(ctx, change.Profile)
		if err != nil {
			return nil, err
		}
		defer profileLock.Close()
	}
	if _, err := os.Lstat(filepath.Join(m.DataRoot, "runtime", "autostart", "maintenance.json")); err == nil {
		return nil, errors.New(i18n.Messagebab3729cac133677)
	} else if !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	if _, _, err := m.Health(ctx); !IsUnavailable(err) {
		return nil, errors.Join(errors.New(i18n.Messagef1bb4a79aedf7709), err)
	}
	_, actual, err := readCoreConfigFile(change.Profile)
	if err != nil {
		return nil, err
	}
	if actual != change.Before {
		return nil, errors.New(i18n.Message862de4bd79329616)
	}
	if err := m.publishDesktopAgentSettings(id, change, settings); err != nil {
		return nil, err
	}
	return offlineCoreConfigResult(change)
}

func (m *Manager) publishDesktopAgentSettings(id string, change *coreConfigChange, settings map[string]json.RawMessage) error {
	encoded, err := json.MarshalIndent(settings, "", "  ")
	if err != nil {
		return err
	}
	change.OfflineFile = fmt.Sprintf("%x", sha256.Sum256(append(encoded, '\n')))
	if err := m.recordCoreConfigChange(id, change); err != nil {
		return err
	}
	if err := writeJSON(filepath.Join(change.Profile, "agent.json"), settings); err != nil {
		return &unconfirmedError{id, err}
	}
	_, actual, err := readCoreConfigFile(change.Profile)
	if err != nil || actual != change.OfflineFile {
		return &unconfirmedError{id, errors.Join(errors.New(i18n.Messagef37780c98798f26f), err)}
	}
	return nil
}

func offlineCoreConfigResult(change *coreConfigChange) (json.RawMessage, error) {
	return json.Marshal(map[string]any{"command": "agent-config", "status": "ok", "summary": map[string]any{
		"action": "updated", "key": change.Key, "value": change.value(),
		"core_application": map[string]string{"state": "pending", "reason": "runtime_not_running"},
	}})
}

func (m *Manager) finishDesktopCoreConfig(ctx context.Context, id string, change *coreConfigChange, raw json.RawMessage) (json.RawMessage, error) {
	result, err := configResult(raw, "applied")
	if err != nil {
		return nil, &unconfirmedError{id, err}
	}
	if *result.Revision == 0 || *result.Revision-1 != change.Revision ||
		!reflect.DeepEqual(result.Candidate, change.Target) {
		return nil, &unconfirmedError{id, errors.New(i18n.Messagecbe317051b6cc395)}
	}
	profile, err := m.coreConfigProfile()
	if err != nil {
		return nil, &unconfirmedError{id, err}
	}
	if !samePath(profile, change.Profile) {
		return nil, &unconfirmedError{id, errors.New(i18n.Messageb892738288bebfd2)}
	}
	scope, err := m.coreConfigScope(ctx, profile)
	if err != nil {
		return nil, &unconfirmedError{id, err}
	}
	if scope != change.Scope {
		return nil, &unconfirmedError{id, errors.New(i18n.Messageb769ab16530ebbdb)}
	}
	settings, digest, err := readCoreConfigFile(profile)
	if err != nil {
		return nil, &unconfirmedError{id, err}
	}
	value, err := json.Marshal(change.value())
	if err != nil {
		return nil, &unconfirmedError{id, err}
	}
	var saved any
	if existing := settings[change.Key]; len(existing) != 0 {
		if err := json.Unmarshal(existing, &saved); err != nil {
			return nil, &unconfirmedError{id, err}
		}
	}
	needsWrite := !reflect.DeepEqual(saved, change.value())
	if needsWrite && digest != change.Before {
		return nil, &unconfirmedError{id, errors.New(i18n.Message862de4bd79329616)}
	}
	current, err := m.requestCoreConfig(ctx, "core-readback-"+rand.Text(), "core_config.read", scope, map[string]any{})
	if err != nil {
		return nil, &unconfirmedError{id, err}
	}
	state, err := configResult(current, "current")
	if err != nil {
		return nil, &unconfirmedError{id, err}
	}
	if *state.Revision != *result.Revision || !reflect.DeepEqual(state.Candidate, result.Candidate) {
		return nil, &unconfirmedError{id, errors.New(i18n.Message2fd7a5da1a53644c)}
	}
	if needsWrite {
		settings[change.Key] = value
		if err := writeJSON(filepath.Join(profile, "agent.json"), settings); err != nil {
			return nil, &unconfirmedError{id, err}
		}
	}
	application := struct {
		coreConfigResult
		Scope runtimeScope `json:"scope"`
	}{result, scope}
	return json.Marshal(map[string]any{"command": "agent-config", "status": "ok", "summary": map[string]any{
		"action": "updated", "key": change.Key, "value": change.value(), "core_application": application,
	}})
}
