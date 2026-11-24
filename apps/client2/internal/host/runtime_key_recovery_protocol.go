//go:build linux || windows || (darwin && cgo)

package host

import "respire-client2/internal/i18n"

import (
	"context"
	"encoding/json"
	"errors"
	"path/filepath"
	"reflect"
)

type runtimeRecoveryOperation struct {
	Kind       string `json:"kind"`
	Server     string `json:"addr"`
	User       string `json:"user"`
	Salt       string `json:"kdf_salt"`
	WrappedURK string `json:"wrapped_urk"`
	Nonce      string `json:"urk_nonce"`
	UseSuper   bool   `json:"use_super"`
}

func (op runtimeRecoveryOperation) MarshalJSON() ([]byte, error) {
	if op.Kind == "super_reset" {
		return json.Marshal(struct {
			Kind string `json:"kind"`
			Addr string `json:"addr"`
		}{op.Kind, op.Server})
	}
	type fivekeys runtimeRecoveryOperation
	return json.Marshal(fivekeys(op))
}

type runtimeRecoveryMaterial struct {
	Account   string                   `json:"keyring_account"`
	Backend   string                   `json:"keyring_backend"`
	Challenge *string                  `json:"challenge,omitempty"`
	Reset     *runtimeRegisterMaterial `json:"reset,omitempty"`
}

type runtimeRecoveryInput struct {
	Password      *string                 `json:"password,omitempty"`
	SuperPassword *string                 `json:"super_password,omitempty"`
	AccountSecret *string                 `json:"account_secret,omitempty"`
	TOTP          *string                 `json:"totp,omitempty"`
	Material      runtimeRecoveryMaterial `json:"material"`
}

type runtimeRecoveryReceipt struct {
	OperationID          string                   `json:"operation_id"`
	Fingerprint          string                   `json:"fingerprint"`
	Operation            runtimeRecoveryOperation `json:"operation"`
	Outcome              string                   `json:"outcome"`
	Source               string                   `json:"source_profile"`
	Target               *string                  `json:"target_profile"`
	AccountID            *string                  `json:"account_id"`
	SessionDigest        *string                  `json:"session_sha256"`
	Identity             runtimeAuthIdentity      `json:"identity"`
	RequiredInputs       []string                 `json:"required_inputs"`
	VerificationRequired bool                     `json:"verification_required"`
	Super                *string                  `json:"super_password"`
	Challenge            *string                  `json:"challenge"`
}

func (m *Manager) exchangeRuntimeRecovery(ctx context.Context, control runtimeAccountControl, record runtimeAccountRecord,
	identity runtimeAuthIdentity, operation runtimeRecoveryOperation, method string, input *runtimeRecoveryInput) (runtimeRecoveryReceipt, error) {
	argument, err := json.Marshal(map[string]any{
		"schema_version": 1, "binding": control.binding, "identity": identity,
		"source_identity": record.Original.Identity, "operation": operation, "input": input,
	})
	if err != nil {
		return runtimeRecoveryReceipt{}, err
	}
	defer clear(argument)
	id, err := controlUUID()
	if err != nil {
		return runtimeRecoveryReceipt{}, err
	}
	response, err := m.exchangeRuntimeControl(ctx, control.image, control.port, control.grant, runtimeControlRequest{
		V: 1, ID: id, Method: method, Args: []string{string(argument)}, OperationID: record.OperationID, IdempotencyKey: record.OperationID,
	})
	if err != nil {
		return runtimeRecoveryReceipt{}, err
	}
	defer clear(response.Envelope)
	return readRuntimeRecoveryReceipt(response, record, operation)
}

func readRuntimeRecoveryReceipt(response rpcResponse, record runtimeAccountRecord, operation runtimeRecoveryOperation) (runtimeRecoveryReceipt, error) {
	var result struct {
		Command string                 `json:"command"`
		Status  string                 `json:"status"`
		Summary runtimeRecoveryReceipt `json:"summary"`
	}
	if json.Unmarshal(response.Envelope, &result) != nil || result.Command != "key-control" || !response.OK {
		return runtimeRecoveryReceipt{}, errors.New(i18n.Message6487e6be625ba90c)
	}
	receipt := result.Summary
	rotation := operation.Kind == "super_reset"
	if receipt.OperationID != record.OperationID || !samePath(receipt.Source, record.Source) ||
		receipt.Operation != operation || !validHash(receipt.Fingerprint) ||
		(!rotation || receipt.Outcome != "committed") && receipt.Super != nil {
		return runtimeRecoveryReceipt{}, errors.New(i18n.Messagebddda7dddaca7fd5)
	}
	switch receipt.Outcome {
	case "awaiting_input":
		if result.Status != "pending" || receipt.VerificationRequired || len(receipt.RequiredInputs) == 0 {
			return runtimeRecoveryReceipt{}, errors.New(i18n.Message9091a640c2e7f7ec)
		}
	case "pending", "committed":
		if receipt.Outcome == "pending" && (!receipt.VerificationRequired || result.Status != "pending") ||
			receipt.Outcome == "committed" && (receipt.VerificationRequired || result.Status != "ok") ||
			receipt.Target == nil || !filepath.IsAbs(*receipt.Target) || receipt.SessionDigest == nil || !validHash(*receipt.SessionDigest) ||
			receipt.Identity.SessionDigest != *receipt.SessionDigest || receipt.Challenge != nil {
			return runtimeRecoveryReceipt{}, errors.New(i18n.Message59077a749a890424)
		}
		if rotation {
			if receipt.Outcome != "committed" || receipt.Super == nil || !samePath(*receipt.Target, record.Source) ||
				receipt.AccountID == nil || record.Original.Identity.AccountID == nil ||
				*receipt.AccountID != *record.Original.Identity.AccountID || receipt.Identity.Kind != "bound" ||
				!reflect.DeepEqual(receipt.Identity.AccountID, receipt.AccountID) {
				return runtimeRecoveryReceipt{}, errors.New(i18n.Message4baa8b5dca926865)
			}
		} else if operation.User == "" {
			if operation.Server != "" || receipt.AccountID != nil || receipt.Identity.Kind != "unbound" ||
				receipt.Identity.OperationID != record.OperationID {
				return runtimeRecoveryReceipt{}, errors.New(i18n.Message4a12ad996a89eaff)
			}
		} else if receipt.AccountID == nil || *receipt.AccountID != controlDigest([]byte(operation.User)) ||
			receipt.Identity.Kind != "bound" || !reflect.DeepEqual(receipt.Identity.AccountID, receipt.AccountID) {
			return runtimeRecoveryReceipt{}, errors.New(i18n.Message5a60c65c96529795)
		}
	default:
		return runtimeRecoveryReceipt{}, errors.New(i18n.Messagecfd896b87f613f8b)
	}
	return receipt, nil
}

func verifyRuntimeRecoveryTarget(record runtimeAccountRecord, receipt runtimeRecoveryReceipt) (target, account string, err error) {
	if receipt.Target == nil || receipt.SessionDigest == nil {
		return "", "", errors.New(i18n.Message33399b79e03e7f99)
	}
	target, err = selectedRuntimeLoginProfile(record.Root)
	if err != nil {
		return "", "", err
	}
	if !samePath(target, *receipt.Target) {
		return "", "", errors.New(i18n.Message325df77a90a50cb1)
	}
	raw, err := readCredentialConfig(filepath.Join(target, "session.json"), controlFrameLimit)
	if err != nil {
		return "", "", err
	}
	defer clear(raw)
	identity, err := runtimeAuthIdentityOf(raw)
	if err != nil {
		return "", "", err
	}
	if !reflect.DeepEqual(identity, receipt.Identity) || identity.SessionDigest != *receipt.SessionDigest {
		return "", "", errors.New(i18n.Message7f20fb9890ec5baa)
	}
	if receipt.AccountID != nil {
		return target, *receipt.AccountID, nil
	}
	return target, controlDigest([]byte(target)), nil
}
