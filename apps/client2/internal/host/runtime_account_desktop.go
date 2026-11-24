//go:build linux || windows || (darwin && cgo)

package host

import "respire-client2/internal/i18n"

import (
	"context"
	"encoding/json"
	"errors"
	"net/url"
	"path/filepath"
	"strings"
)

func (m *Manager) desktopRuntimeAccountLogin(ctx context.Context, args []string, secret, id, method string) (envelope json.RawMessage, err error) {
	browser, register := method == "auth.login.browser", method == "auth.register"
	input, err := authParams(args, secret)
	if err != nil {
		return nil, err
	}
	operation := strings.TrimPrefix(id, "desktop-")
	user, password := input["user"], input["password"]
	if !hostOperationID.MatchString(operation) || m.ClientVersion == "" {
		return nil, errors.New(i18n.Messagefb80de0c0f76f7b9)
	}
	if (browser && secret != "") || (!browser && (user == "" || password == "")) {
		return nil, errors.New(i18n.Messagec6c45e62c8466715)
	}
	server := input["addr"]
	if server == "" {
		server, err = m.DeviceServerAddress()
		if err != nil {
			return nil, err
		}
	}
	address, err := url.Parse(server)
	if err != nil || address.Hostname() == "" || (address.Scheme != "https" && address.Scheme != "http") ||
		address.User != nil || address.RawQuery != "" || address.Fragment != "" {
		return nil, errors.New(i18n.Messagec67768c063143031)
	}
	authorization := runtimeLoginAuthorization{Kind: "password", Password: &password}
	if totp := input["totp"]; totp != "" {
		authorization.TOTP = &totp
	}
	if browser {
		user, authorization, err = m.runtimeBrowserAuthorization(ctx, strings.TrimSuffix(server, "/"))
		if err != nil {
			return nil, err
		}
		server = strings.TrimSuffix(server, "/")
	}
	takeover, err := m.lockRuntimeTakeover(ctx)
	if err != nil {
		return nil, err
	}
	defer func() { err = errors.Join(err, takeover.Close()) }()
	info, err := m.Installed()
	if err != nil {
		return nil, err
	}
	if err := m.verifyInstalled(info); err != nil {
		return nil, err
	}
	root, err := canonicalSessionDirectory(m.DataRoot)
	if err != nil {
		return nil, err
	}
	profile, err := selectedRuntimeLoginProfile(root)
	if err != nil {
		return nil, err
	}
	// Record the native transaction before Runtime can act on it.
	var source, target json.RawMessage
	var registration runtimeRegisterMaterial
	if register {
		registration, err = prepareDesktopRegistration(ctx, operation, profile, input["super"])
	} else {
		source, err = readRuntimeLoginMaterial(ctx, profile)
		if err == nil {
			target, err = readRuntimeLoginTargetMaterial(ctx, root, profile, user, server)
		}
	}
	defer clear(source)
	defer clear(target)
	if err != nil {
		return nil, err
	}
	if err := m.bindDesktopRuntimeLogin(id, operation, profile, register); err != nil {
		return nil, err
	}
	var previous *HealthInfo
	health, _, err := m.Health(ctx)
	if err == nil {
		if err := m.verifySelectedHealth(health, info); err != nil {
			return nil, err
		}
		if health.ManagedID != "" {
			return nil, errors.New(i18n.Messagef288421a46077817)
		}
		if !samePath(health.DataDir, profile) || health.ImageSHA256 != info.SHA256 {
			return nil, errors.New(i18n.Message28d1aa79c95a9b79)
		}
		if err := m.stopDetached(ctx, health); err != nil {
			return nil, err
		}
		previous = &health
	} else if !IsUnavailable(err) {
		return nil, err
	}
	intent := runtimeLoginIntent{OperationID: operation, User: user, Server: server, Register: register}
	admission, err := m.publishPausedRuntimeMaintenance(ctx, profile, m.ClientVersion, intent, previous)
	if err != nil {
		return nil, err
	}
	unknown := func(cause error) (json.RawMessage, error) {
		receipt := runtimeLoginReceipt{OperationID: operation, SourceProfile: profile, Outcome: "unknown"}
		raw, err := desktopRuntimeLoginEnvelope(receipt)
		return raw, &unconfirmedError{id: operation, cause: errors.Join(cause, err)}
	}
	var super *string
	if value := input["super"]; value != "" {
		super = &value
	}
	image := filepath.Join(m.runtimeDir(info.Version), runtimeBinary())
	var result runtimeLoginResult
	if register {
		result, err = m.sendDesktopRegistration(ctx, image, info.SHA256, &admission, registration, password)
	} else {
		var control runtimeAccountControl
		var params runtimeLoginParams
		control, params, err = m.prepareInitialRuntimeLogin(ctx, image, info.SHA256, admission, authorization,
			runtimeLoginContinuation{OfferLocalMerge: true}, super, source, target)
		if err == nil {
			result, err = m.sendInitialRuntimeLogin(ctx, control, params, &admission)
		}
	}
	if err != nil {
		if register && admission.Path == "" {
			return nil, err
		}
		return unknown(err)
	}
	raw, err := m.finishDesktopRuntimeLogin(ctx, image, info.SHA256, &admission, result.Receipt)
	if err != nil && len(raw) == 0 {
		return unknown(err)
	}
	return raw, err
}

func (m *Manager) bindDesktopRuntimeLogin(id, operation, profile string, register bool) error {
	m.mu.Lock()
	defer m.mu.Unlock()
	job := m.jobs[id]
	if job == nil || job.State != "running" || job.Transaction != nil {
		return errors.New(i18n.Messageed9560c1b89afd73)
	}
	job.Transaction = &hostTransaction{
		OperationID: operation, SourceProfile: profile, Kind: "account", RuntimeLogin: true, Register: register,
	}
	if err := m.persistOperationsLocked(); err != nil {
		job.Transaction = nil
		return err
	}
	return nil
}

func (m *Manager) finishDesktopRuntimeLogin(ctx context.Context, image, digest string, admission *runtimeAccountAdmission, receipt runtimeLoginReceipt) (json.RawMessage, error) {
	switch receipt.Outcome {
	case "awaiting_input":
		raw, err := desktopRuntimeLoginEnvelope(receipt)
		return raw, &unconfirmedError{id: receipt.OperationID, cause: errors.Join(errors.New(i18n.Message1b9c9a0e0e2b9b95), err)}
	case "awaiting_verification":
		committed, err := m.startAndCompleteRuntimeLogin(ctx, image, digest, admission, receipt)
		if err != nil {
			return nil, err
		}
		receipt = committed
	}
	return desktopRuntimeLoginEnvelope(receipt)
}

func desktopRuntimeLoginEnvelope(receipt runtimeLoginReceipt) (json.RawMessage, error) {
	status := "ok"
	switch receipt.Outcome {
	case "awaiting_input":
		status = "pending"
	case "unknown":
		status = "fail"
	}
	receipt.SuperPassword = nil
	if receipt.Login != nil {
		receipt.Login.SuperIssued = nil
	}
	summary := struct {
		runtimeLoginReceipt
		RuntimeLogin bool `json:"runtime_login"`
	}{receipt, true}
	return json.Marshal(map[string]any{"command": "auth-account", "status": status, "summary": summary, "items": []any{}, "errors": []string{}})
}
