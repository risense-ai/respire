package host

import "respire-client2/internal/i18n"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"regexp"
)

// Only the supervising host can pause its own restart policy.
func (m *Manager) stopManaged(ctx context.Context, h HealthInfo) error {
	if h.InstallationRoot == "" || !filepath.IsAbs(h.InstallationRoot) {
		return errors.New(i18n.Message49ba9c7d11f1110f)
	}
	if err := verifyHostProcess(h); err != nil {
		return err
	}
	if err := verifyExecutable(h.Exe); err != nil {
		return err
	}
	if h.ManagedID != "" {
		return errors.New(i18n.Messagecbef8ccc12ad43e6)
	}
	lock, err := m.lockRuntimeTakeover(ctx)
	if err != nil {
		return err
	}
	defer lock.Close()
	if err := m.stopDetached(ctx, h); err != nil {
		return err
	}
	_, _, err = m.Health(ctx)
	if IsUnavailable(err) {
		return nil
	}
	if err != nil {
		return err
	}
	return errors.New(i18n.Message7612dd2e3c38e61f)
}

var runtimeInstanceID = regexp.MustCompile(`^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`)

func (m *Manager) stopDetached(ctx context.Context, h HealthInfo) error {
	if !samePath(h.InstallationRoot, m.DataRoot) || !validHash(h.InstallationID) ||
		!runtimeInstanceID.MatchString(h.InstanceID) || !validHash(h.AccountID) || h.Generation == 0 ||
		h.ProcessStartID == "" || !validHash(h.ImageSHA256) || !filepath.IsAbs(h.ImagePath) ||
		h.RuntimeVersion != h.Version || !validVersion(h.RuntimeVersion) {
		return errors.New(i18n.Message233068eb02d0163d)
	}
	executable, err := os.Stat(h.Exe)
	if err != nil {
		return err
	}
	image, err := os.Stat(h.ImagePath)
	if err != nil {
		return err
	}
	if !os.SameFile(executable, image) {
		return errors.New(i18n.Message24f9191f4854f1c5)
	}
	var accepted struct {
		OK          bool   `json:"ok"`
		State       string `json:"state"`
		OperationID string `json:"operation_id"`
	}
	if err := m.stopProcess(ctx, h, func() error {
		raw, err := m.localRequest(ctx, "POST", "/api/runtime/stop", map[string]any{
			"installation_id": h.InstallationID, "instance_id": h.InstanceID,
			"account_id": h.AccountID, "generation": h.Generation,
		})
		if err != nil {
			return fmt.Errorf(i18n.Message81ae7d15fbf48aea, err)
		}
		if err := json.Unmarshal(raw, &accepted); err != nil {
			return err
		}
		if !accepted.OK || accepted.State != "accepted" || !runtimeInstanceID.MatchString(accepted.OperationID) {
			return errors.New(i18n.Message6fc3fbb44b9ca501)
		}
		return nil
	}); err != nil {
		return err
	}
	path := filepath.Join(m.DataRoot, "runtime", "stop-"+h.InstanceID+".json")
	info, err := os.Lstat(path)
	if err != nil {
		return fmt.Errorf(i18n.Message69d8e14d6187da28, err)
	}
	if !info.Mode().IsRegular() || info.Size() > 1<<20 {
		return errors.New(i18n.Message133c9525411e28ea)
	}
	file, err := os.Open(path)
	if err != nil {
		return err
	}
	defer file.Close()
	raw, err := io.ReadAll(io.LimitReader(file, 1<<20+1))
	if err != nil {
		return err
	}
	var receipt struct {
		HealthInfo
		SchemaVersion int               `json:"schema_version"`
		Protocol      int               `json:"protocol_version"`
		OperationID   string            `json:"operation_id"`
		State         string            `json:"state"`
		Unfinished    []json.RawMessage `json:"unfinished"`
	}
	if len(raw) > 1<<20 {
		return errors.New(i18n.Message1ebcf0a0cde20a06)
	}
	if err := json.Unmarshal(raw, &receipt); err != nil {
		return err
	}
	if receipt.SchemaVersion != 1 || receipt.Protocol != 1 || receipt.OperationID != accepted.OperationID ||
		receipt.InstallationID != h.InstallationID || receipt.InstanceID != h.InstanceID ||
		receipt.AccountID != h.AccountID || receipt.Generation != h.Generation || receipt.PID != h.PID ||
		receipt.ProcessStartID != h.ProcessStartID || receipt.ImageSHA256 != h.ImageSHA256 ||
		!samePath(receipt.ImagePath, h.ImagePath) || receipt.RuntimeVersion != h.RuntimeVersion ||
		receipt.State != "drained" || receipt.Unfinished == nil || len(receipt.Unfinished) != 0 {
		return errors.New(i18n.Message44da4b1f8a6dbe02)
	}
	return nil
}
