//go:build linux || windows || (darwin && cgo)

package host

import "respire-client2/internal/i18n"

import (
	"context"
	"crypto/hkdf"
	"crypto/pbkdf2"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"net/url"
	"path/filepath"
	"strings"

	"golang.org/x/crypto/argon2"
)

func prepareDesktopKeyReset(ctx context.Context, profile, op string, input map[string]string) (recoveryCustody, string, error) {
	var custody recoveryCustody
	for key := range input {
		if key != "super" && key != "password" {
			return custody, "", errors.New(i18n.Messagef8f13178b6737252)
		}
	}
	raw, err := readCredentialConfig(filepath.Join(profile, "session.json"), controlFrameLimit)
	if err != nil {
		return custody, "", err
	}
	defer clear(raw)
	var vault exportVault
	if err := json.Unmarshal(raw, &vault); err != nil {
		return custody, "", err
	}
	var session struct {
		Token string `json:"token"`
	}
	if json.Unmarshal(raw, &session) != nil || session.Token == "" || vault.User == "" {
		return custody, "", errors.New(i18n.Messagecdc472bcc1134ba9)
	}
	server := strings.TrimRight(strings.TrimSpace(vault.Server), "/")
	if server == "" {
		server = "https://api.rsrs.rs"
	}
	addr, err := url.Parse(server)
	if err != nil || addr.Hostname() == "" || (addr.Scheme != "https" && addr.Scheme != "http") || addr.User != nil || addr.RawQuery != "" || addr.Fragment != "" {
		return custody, "", errors.New(i18n.Messageed1f48cc9050442a)
	}
	old, err := resetVaultFactors(nativeRuntimeCredentialStore(ctx), vault, input)
	if err != nil {
		return custody, "", err
	}
	urk, err := vault.unwrapWithOriginalFactors(old)
	if err != nil {
		return custody, "", err
	}
	defer clear(urk)
	replacement, err := wrapRuntimeVaultKey(op, urk)
	if err != nil {
		return custody, "", err
	}
	old.Material = runtimeRecoveryMaterial{Account: replacement.Account, Backend: replacement.Backend, Reset: &replacement}
	return recoveryCustody{Operation: runtimeRecoveryOperation{Kind: "super_reset", Server: server}, Input: old}, vault.User, nil
}

func resetVaultVersion(vault exportVault) uint64 {
	if vault.Version == nil {
		return 1
	}
	return *vault.Version
}

func resetVaultFactors(store hostCredentialStore, vault exportVault, input map[string]string) (runtimeRecoveryInput, error) {
	var result runtimeRecoveryInput
	account, err := vault.credentialAccount()
	if err != nil {
		return result, err
	}
	read := func(value *string, backend, slot string, legacy bool) (*string, error) {
		if value == nil || *value == "" {
			var err error
			value, err = store.readSlot(backend, "rsrs", slot+":"+account, legacy)
			if err != nil {
				return nil, err
			}
		}
		if value == nil || *value == "" {
			return nil, errors.New(i18n.Message79a17e16261920d5)
		}
		return value, nil
	}
	version := resetVaultVersion(vault)
	if version < 1 || version > 4 {
		return result, errors.New(i18n.Message2fd2db7187106256)
	}
	if version == 1 {
		password := vault.LegacyPass
		if supplied := input["password"]; supplied != "" {
			password = &supplied
		}
		result.Password, err = read(password, vault.Backend, "pass", true)
	} else {
		super := vault.LegacySuper
		if supplied := input["super"]; supplied != "" {
			super = &supplied
		}
		if version == 4 && super == nil {
			super = vault.SecretKey
		}
		result.SuperPassword, err = read(super, vault.Backend, "super", false)
	}
	if err != nil {
		return result, err
	}
	if version == 1 || version == 3 {
		legacy := vault.LegacySecret
		if version == 3 {
			legacy = vault.SecretKey
		}
		result.AccountSecret, err = read(legacy, vault.LegacyBackend, "legacy-secret", false)
	}
	return result, err
}

func (vault exportVault) unwrapWithOriginalFactors(input runtimeRecoveryInput) ([]byte, error) {
	version := resetVaultVersion(vault)
	if version == 4 {
		return vault.unwrapSuperKey(*input.SuperPassword)
	}
	salt, err := hex.DecodeString(vault.Salt)
	if err != nil || version == 1 && len(salt) < 8 {
		return nil, errors.New(i18n.Messageca4904110a929434)
	}
	var key []byte
	if version == 1 {
		key = argon2.IDKey([]byte(*input.Password), salt, 2, 19456, 1, 32)
	} else {
		key, err = pbkdf2.Key(sha256.New, *input.SuperPassword, salt, 210000, 32)
		if err != nil {
			return nil, err
		}
	}
	defer clear(key)
	if version == 1 || version == 3 {
		domain := "onememory:kek:v1"
		if strings.HasPrefix(vault.Wrapped, "rsrs:v1:") {
			domain = "rsrs:kek:v1"
		}
		combined, err := hkdf.Key(sha256.New, key, []byte(*input.AccountSecret), domain, 32)
		if err != nil {
			return nil, err
		}
		defer clear(combined)
		return vault.unwrapVaultKey(combined)
	}
	return vault.unwrapVaultKey(key)
}

func saveDesktopKeyResetReceipt(record runtimeAccountRecord, receipt runtimeRecoveryReceipt) error {
	path := loginCustodyPath(record)
	old, err := readCredentialConfig(path, runtimeMaintenanceLimit)
	if err != nil {
		return err
	}
	var archive map[string]json.RawMessage
	if json.Unmarshal(old, &archive) != nil || archive == nil {
		return errors.New(i18n.Message3d25ce2ccd017038)
	}
	var operation string
	if json.Unmarshal(archive["operation_id"], &operation) != nil || operation != record.OperationID {
		return errors.New(i18n.Messagea01fb028da2ba3b7)
	}
	receipt.Super = nil
	encoded, err := json.Marshal(receipt)
	if err != nil {
		return err
	}
	archive["reset_receipt"] = encoded
	revised, err := json.Marshal(archive)
	if err != nil {
		return err
	}
	return writeRuntimeAccountRecord(path, revised, old)
}

func readDesktopKeyResetReceipt(ctx context.Context, record runtimeAccountRecord) (runtimeRecoveryReceipt, error) {
	var receipt runtimeRecoveryReceipt
	raw, err := readCredentialConfig(loginCustodyPath(record), runtimeMaintenanceLimit)
	if err != nil {
		return receipt, err
	}
	var archive struct {
		OperationID string                 `json:"operation_id"`
		Receipt     runtimeRecoveryReceipt `json:"reset_receipt"`
	}
	if json.Unmarshal(raw, &archive) != nil || archive.OperationID != record.OperationID {
		return receipt, errors.New(i18n.Message6ced468030e0c6c9)
	}
	receipt = archive.Receipt
	if receipt.Outcome != "committed" || receipt.OperationID != record.OperationID || receipt.Operation.Kind != "super_reset" ||
		receipt.Operation.Server != record.Server || !validHash(receipt.Fingerprint) || !samePath(receipt.Source, record.Source) ||
		receipt.Target == nil || !samePath(*receipt.Target, record.Source) || receipt.Super != nil {
		return receipt, errors.New(i18n.Message54a90c2607933da7)
	}
	profile, _, err := verifyRuntimeRecoveryTarget(record, receipt)
	if err != nil {
		return receipt, err
	}
	session, err := readCredentialConfig(filepath.Join(profile, "session.json"), controlFrameLimit)
	if err != nil {
		return receipt, err
	}
	defer clear(session)
	if controlDigest(session) != *receipt.SessionDigest {
		return receipt, errors.New(i18n.Message00c8ff115ba71036)
	}
	var vault exportVault
	alias := "login-" + strings.ReplaceAll(record.OperationID, "-", "")
	if err := json.Unmarshal(session, &vault); err != nil || vault.Alias != alias || vault.Backend != runtimeLoginBackend() {
		return receipt, errors.New(i18n.Message7cc3ee7f32490201)
	}
	super, err := nativeRuntimeCredentialStore(ctx).readSlot(vault.Backend, "rsrs", "super:"+alias, false)
	if err != nil {
		return receipt, err
	}
	if super == nil || *super == "" {
		return receipt, errors.New(i18n.Messagedbfca547daaee2ad)
	}
	if err := vault.verifyExportSuper(*super); err != nil {
		return receipt, err
	}
	receipt.Super = super
	return receipt, nil
}
