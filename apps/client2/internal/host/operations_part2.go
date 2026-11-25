package host

import (
	"context"
	"encoding/json"
	"errors"
	"respire-client2/internal/i18n"
	"time"
)

func (m *Manager) inspectResult(ctx context.Context, id string, scope *runtimeScope, original error) (json.RawMessage, error) {
	if scope == nil || scope.AccountID == "" || scope.InstanceID == "" || scope.Generation == 0 {
		return nil, &unconfirmedError{id, errors.New(i18n.Message182a38bf745d569e)}
	}
	for {
		reply, err := m.rpcResponse(ctx, "runtime.result", []string{id}, "inspect-"+id, scope)
		if err != nil {
			return nil, &unconfirmedError{id, errors.Join(original, err)}
		}
		if err = responseError(reply); err != nil {
			return reply.Envelope, &unconfirmedError{id, err}
		}
		var envelope struct {
			Summary struct {
				State    string       `json:"state"`
				Response *rpcResponse `json:"response"`
			} `json:"summary"`
		}
		if err = json.Unmarshal(reply.Envelope, &envelope); err != nil {
			return nil, &unconfirmedError{id, err}
		}
		switch envelope.Summary.State {
		case "completed", "failed":
			result := envelope.Summary.Response
			if result == nil || result.V != 1 || result.ID != id || result.Scope == nil || *result.Scope != *scope || result.OperationID != id || (envelope.Summary.State == "completed") != result.OK {
				return nil, &unconfirmedError{id, errors.New(i18n.Messagef61941c606eb26a2)}
			}
			return result.Envelope, responseError(*result)
		case "pending":
			select {
			case <-ctx.Done():
				return nil, &unconfirmedError{id, ctx.Err()}
			case <-time.After(time.Second):
			}
		default:
			return nil, &unconfirmedError{id, original}
		}
	}
}

// Recover inspects a saved request; it never submits its command again.
func (m *Manager) Recover(id string) (Operation, error) {
	m.mu.Lock()
	original := m.jobs[id]
	if original == nil {
		m.mu.Unlock()
		return Operation{}, errors.New(i18n.Message1365397b075962ba)
	}
	job := *original
	if job.State != "unconfirmed" && !job.materialPending() &&
		!(job.Name == "keygen" && job.State == "done" && len(job.Envelope) == 0) {
		m.mu.Unlock()
		return job, nil
	}
	original.State = "running"
	original.inputBlocked = true
	m.mu.Unlock()
	var err error
	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	// Host result queries can drain and restore Runtime and verify native custody.
	// Like account submissions, let the CLI own their lifecycle deadlines.
	if !job.host {
		timedCtx, timedCancel := context.WithTimeout(ctx, 30*time.Second)
		defer timedCancel()
		ctx = timedCtx
	}
	var envelope json.RawMessage
	switch {
	case job.Name == "logout", job.Name == "logout_full":
		envelope, err = m.desktopRuntimeLogout(ctx, id, job.Name == "logout_full", true)
	case job.Name == "server_set", job.Name == "autosync_set", job.Name == "cure_auto_set", job.Name == "data_dir_set",
		job.Name == "account_use", job.Name == "space_use":
		err = &unconfirmedError{id, errors.New(i18n.Messagefc88d9a20258efca)}
	case job.Name == "space_create":
		err = &unconfirmedError{id, errors.New(i18n.Message420aae91b4c71eef)}
	case job.Name == "keygen":
		envelope, _, err = m.desktopOfflineKeygen(ctx, id, true)
	case job.Name == "agent_set" && job.host && job.Transaction == nil:
		err = &unconfirmedError{id, errors.New(i18n.Messaged628c150d2ffc330)}
	case job.Name == "agent_set" && !job.host:
		envelope, err = m.recoverDesktopCoreConfig(ctx, job)
	case job.Name == "model_uninstall":
		err = &unconfirmedError{id, errors.New(i18n.Message5edab80dfd637743)}
	case job.Name == "migration_apply":
		envelope, err = m.desktopLegacyMigration(ctx, id, nil, true)
	case job.Name == "space_kick", job.Name == "space_kick_all":
		envelope, err = m.desktopSpaceMembership(ctx, id, nil, "kick", job.Name, hostCredentialStore{})
	case job.Name == "space_invite":
		envelope, err = m.desktopSpaceMembership(ctx, id, nil, "invite", job.Name, nativeRuntimeCredentialStore(ctx))
	case job.Name == "space_remove":
		envelope, err = m.removeDesktopSpace(ctx, id, "", true)
	case job.Name == "space_join":
		envelope, err = m.desktopSpaceMembership(ctx, id, nil, "join", job.Name, nativeRuntimeCredentialStore(ctx))
	case job.host:
		envelope, err = m.recoverHost(ctx, job)
	default:
		runtimeID := job.RuntimeID
		if runtimeID == "" {
			runtimeID = id
		}
		envelope, err = m.inspectResult(ctx, runtimeID, job.Scope, errors.New(i18n.Message1627c7136221a7c5))
		if err == nil && job.Name == "model_engine_apply" {
			err = &unconfirmedError{id, errors.New(i18n.Message9b571edfa20ff2a0)}
		}
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	j := m.jobs[id]
	if len(envelope) > 0 {
		if j.Transaction != nil && j.Transaction.RequestBound {
			returned := transactionFromEnvelope(j.Name, envelope)
			if j.Transaction.acceptsCoreResult(returned) {
				returned.RequestBound = true
				j.Transaction = returned
			}
		}
		j.Envelope = envelope
		j.inputBlocked = false
	}
	if err == nil {
		j.State = "done"
		j.Error = ""
	} else {
		j.Error = err.Error()
		var unknown *unconfirmedError
		if errors.As(err, &unknown) {
			unknown.id = id
			j.State, j.Error = "unconfirmed", err.Error()
		} else {
			j.State = "failed"
		}
	}
	if err = m.persistOperationsLocked(); err != nil {
		return *j, err
	}
	copy := *j
	copy.Input = pendingHostInput(copy)
	copy.MaterialPending = copy.materialPending()
	return copy, nil
}
