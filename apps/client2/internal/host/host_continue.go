package host

import "respire-client2/internal/i18n"

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"
)

type hostInputRequest struct {
	Method         string   `json:"method"`
	RequiredInputs []string `json:"required_inputs"`
	User           string   `json:"user,omitempty"`
	Addr           string   `json:"addr,omitempty"`
}

func pendingHostInput(job Operation) *hostInputRequest {
	if job.State != "unconfirmed" || job.inputBlocked || job.Transaction == nil || job.Transaction.Register ||
		(!job.Transaction.RuntimeLogin && !job.Transaction.RuntimeKey) {
		return nil
	}
	var envelope struct {
		Summary struct {
			Outcome        string   `json:"outcome"`
			RequiredInputs []string `json:"required_inputs"`
			Login          *struct {
				RequiredInputs []string `json:"required_inputs"`
				User           string   `json:"user"`
				Addr           string   `json:"addr"`
			} `json:"login"`
		} `json:"summary"`
	}
	if json.Unmarshal(job.Envelope, &envelope) != nil || envelope.Summary.Outcome != "awaiting_input" {
		return nil
	}
	if job.Transaction.Kind == "account" && envelope.Summary.Login != nil {
		login := envelope.Summary.Login
		return &hostInputRequest{"auth.login.continue", login.RequiredInputs, login.User, login.Addr}
	}
	if job.Transaction.Kind == "key" {
		return &hostInputRequest{Method: "auth.key.continue", RequiredInputs: envelope.Summary.RequiredInputs}
	}
	return nil
}

// Continue keeps the original desktop job and uses a separate one-shot transport ID.
func (m *Manager) Continue(id, transportID string, input json.RawMessage) (Operation, error) {
	if !operationID.MatchString(transportID) || transportID == id {
		return Operation{}, errors.New(i18n.Messageb3fa21f79bd87273)
	}
	if len(input) > 49152 || !json.Valid(input) || !bytes.HasPrefix(bytes.TrimSpace(input), []byte("{")) {
		return Operation{}, errors.New(i18n.Message43818847090888a2)
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	job := m.jobs[id]
	if job == nil || !job.host || job.State != "unconfirmed" || job.Transaction == nil {
		return Operation{}, errors.New(i18n.Message94f2fc74853e692a)
	}
	if job.TransportID == transportID {
		return Operation{}, errors.New(i18n.Messagefa3fefb5a3fc035f)
	}
	if !job.Transaction.RuntimeLogin && !job.Transaction.RuntimeKey {
		return Operation{}, errors.New(i18n.Message538723ce2fbe36de)
	}
	if pendingHostInput(*job) == nil {
		return Operation{}, errors.New(i18n.Message59b12e2464322b5a)
	}
	previous := *job
	job.State, job.TransportID, job.Error, job.Finished = "running", transportID, "", nil
	job.inputBlocked = true
	if err := m.persistOperationsLocked(); err != nil {
		*job = previous
		return Operation{}, err
	}
	go m.runHostContinuation(previous, input)
	return *job, nil
}

func continuationParams(job Operation, input json.RawMessage) (string, map[string]any, error) {
	request := pendingHostInput(job)
	if request == nil || len(request.RequiredInputs) == 0 {
		return "", nil, errors.New(i18n.Message19272c7787f14e3f)
	}
	var supplied map[string]json.RawMessage
	if err := json.Unmarshal(input, &supplied); err != nil {
		return "", nil, err
	}
	requested := map[string]bool{}
	for _, field := range request.RequiredInputs {
		if requested[field] {
			return "", nil, errors.New(i18n.Messagebb50379bfcd584a6)
		}
		requested[field] = true
	}
	params := map[string]any{"operation_id": job.Transaction.OperationID, "source_profile": job.Transaction.SourceProfile}
	values := map[string]any{}
	for field, raw := range supplied {
		if !requested[field] {
			return "", nil, fmt.Errorf(i18n.Message5460d94b457fd95d, field)
		}
		var value any
		if field == "merge_local" && request.Method == "auth.login.continue" {
			var merge bool
			if err := json.Unmarshal(raw, &merge); err != nil || string(raw) == "null" {
				return "", nil, errors.New(i18n.Message0e4a4ad8339b6b45)
			}
			value = merge
		} else {
			var text string
			if json.Unmarshal(raw, &text) != nil || text == "" || len(text) > 16384 || strings.ContainsRune(text, 0) {
				return "", nil, errors.New(i18n.Messageb03da6e0ac19d533)
			}
			if field == "totp" && (len(text) != 6 || strings.IndexFunc(text, func(c rune) bool { return c < '0' || c > '9' }) >= 0) {
				return "", nil, errors.New(i18n.Message76a79de07a7a316f)
			}
			value = text
		}
		if request.Method == "auth.login.continue" {
			switch field {
			case "super_password", "totp":
				params[field] = value
			case "legacy_password", "account_secret", "source_secret", "secret_key", "new_super", "merge_local":
				values[field] = value
			default:
				return "", nil, errors.New(i18n.Messageb045f4658eaaba51)
			}
		} else {
			switch field {
			case "password", "super_password", "account_secret", "totp":
				values[field] = value
			default:
				return "", nil, errors.New(i18n.Message73916173f5b694da)
			}
		}
	}
	for field := range requested {
		if field == "host_credentials" && request.Method == "auth.login.continue" {
			continue // The host reads its stored credentials.
		}
		if _, ok := supplied[field]; !ok {
			return "", nil, fmt.Errorf(i18n.Message5dfeab6f7b155bbe, field)
		}
	}
	if request.Method == "auth.login.continue" {
		params["continuation"] = values
	} else {
		params["input"] = values
	}
	return request.Method, params, nil
}

func (m *Manager) runHostContinuation(job Operation, input json.RawMessage) {
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	raw, err := m.recoverHost(ctx, job)
	confirmedInput := false
	var unknown *unconfirmedError
	if len(raw) > 0 && (err == nil || errors.As(err, &unknown)) {
		current := job
		current.Envelope = raw
		if pendingHostInput(current) != nil {
			var reply json.RawMessage
			var continueErr error
			if job.Transaction.RuntimeKey {
				reply, continueErr = m.resumeDesktopRuntimeRecovery(ctx, current, input)
			} else {
				reply, continueErr = m.continueDesktopRuntimeLogin(ctx, current, input)
			}
			err = continueErr
			if len(reply) != 0 {
				returned := transactionFromEnvelope(job.Name, reply)
				if returned == nil || *returned != *job.Transaction {
					err = &unconfirmedError{job.ID, errors.New(i18n.Messaged5c7e5ac79e6b5d4)}
				} else {
					raw = reply
					confirmedInput = true
				}
			}
		}
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	j := m.jobs[job.ID]
	j.inputBlocked = !confirmedInput
	if len(raw) > 0 {
		j.Envelope = raw
	}
	finished := time.Now().UTC()
	j.Finished, j.State, j.Error = &finished, "done", ""
	if err != nil {
		j.State, j.Error = "failed", err.Error()
		if errors.As(err, &unknown) {
			unknown.id = job.ID
			j.State, j.Error = "unconfirmed", err.Error()
		}
	}
	if saveErr := m.persistOperationsLocked(); saveErr != nil {
		j.Error = errors.Join(err, saveErr).Error()
	}
}
