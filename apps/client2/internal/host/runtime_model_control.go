package host

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"strconv"
	"time"
)

var errOnlineModelUnavailable = errors.New("Runtime could not verify a managed local model for online uninstall")
var errOnlineModelUnconfirmed = errors.New("Runtime did not confirm the model operation. Check model status before retrying")

type onlineModelReceipt struct {
	ModelID       string  `json:"model_id"`
	Outcome       string  `json:"outcome"`
	ActiveChanged *bool   `json:"active_changed"`
	Detached      int     `json:"detached_files"`
	Deleted       int     `json:"deleted_files"`
	Cleanup       *string `json:"cleanup_error"`
}

// Runtime itself checks the listening process, installed endpoint receipt,
// active account, profile, and owner-only native credential before sending
// the model operation. The Go host never reads or handles the owner secret.
func (m *Manager) removeRuntimeOwnedModel(ctx context.Context, health HealthInfo, modelID string) (onlineModelReceipt, error) {
	raw, err := m.runRuntimeModelControl(ctx, health, modelID, nil)
	if err != nil {
		return onlineModelReceipt{}, err
	}
	return decodeOnlineModelReceipt(raw, modelID)
}

// Only byte sources opened by Client2's own download path may be passed here;
// the native helper receives neither a path nor the ability to choose a mirror.
func (m *Manager) installRuntimeOwnedModel(ctx context.Context, health HealthInfo, modelID string, weights, tokenizer io.Reader) error {
	if weights == nil || tokenizer == nil {
		return errOnlineModelUnavailable
	}
	raw, err := m.runRuntimeModelControl(ctx, health, modelID, io.MultiReader(weights, tokenizer))
	if err != nil {
		return err
	}
	var response struct {
		Status int `json:"http_status"`
		Body   struct {
			ModelID string `json:"model_id"`
			Outcome string `json:"outcome"`
		} `json:"body"`
	}
	if len(raw) > 8192 || json.Unmarshal(raw, &response) != nil {
		return errOnlineModelUnconfirmed
	}
	switch {
	case response.Status == http.StatusRequestTimeout || response.Status >= http.StatusInternalServerError:
		return errOnlineModelUnconfirmed
	case response.Status != http.StatusCreated:
		return errOnlineModelUnavailable
	}
	if response.Body.ModelID != modelID || response.Body.Outcome != "installed" {
		return errOnlineModelUnconfirmed
	}
	return nil
}

func (m *Manager) runRuntimeModelControl(ctx context.Context, health HealthInfo, modelID string, transfer io.Reader) ([]byte, error) {
	release, err := m.holdProfile()
	if err != nil {
		return nil, err
	}
	defer release()
	if !health.StartupComplete || !health.StoreReady || health.PID == 0 ||
		health.AccountID == "" || health.InstanceID == "" || health.ProcessStartID == "" || health.Generation == 0 {
		return nil, errOnlineModelUnavailable
	}
	selected, err := m.Installed()
	if err != nil {
		return nil, err
	}
	if err := m.verifyRuntimeSelection(selected); err != nil {
		return nil, err
	}
	if err := m.verifySelectedHealth(health, selected); err != nil {
		return nil, err
	}
	_, port, err := m.runtimeAccountEndpoint()
	if err != nil {
		return nil, err
	}
	image := filepath.Join(m.runtimeDir(selected.Version), runtimeBinary())
	portNumber := strconv.FormatUint(port, 10)
	args, err := json.Marshal(map[string]any{
		"schema_version": 1,
		"pid":            health.PID, "process_start_id": health.ProcessStartID,
		"instance_id": health.InstanceID, "account_id": health.AccountID,
		"generation": health.Generation, "model_id": modelID,
	})
	if err != nil || len(args) > 4096 {
		return nil, errOnlineModelUnavailable
	}
	flag, limit := "--model-control-internal", 160*time.Second
	var input io.Reader = bytes.NewReader(args)
	if transfer != nil {
		flag, limit = "--model-install-internal", 205*time.Second
		input = io.MultiReader(bytes.NewReader(append(args, '\n')), transfer)
	}
	scope, cancel := context.WithTimeout(ctx, limit)
	defer cancel()
	cmd := exec.CommandContext(scope, image, flag)
	cmd.Dir = filepath.Dir(image)
	configureCommand(cmd)
	var env []string
	for _, name := range []string{"SystemRoot", "WINDIR", "PATH", "TMP", "TEMP", "HOME", "USERPROFILE"} {
		if value, present := os.LookupEnv(name); present {
			env = append(env, name+"="+value)
		}
	}
	cmd.Env = append(env, "RSRS_DATA_DIR="+m.DataRoot, "RSRS_RPC_PORT="+portNumber)
	cmd.Stdin = input
	response := &onlineModelOutput{}
	cmd.Stdout = response
	// The child receives process scope only. Never expose raw stderr or an
	// unchecked server error to the app. A transport failure is not a retry.
	if err := cmd.Run(); err != nil {
		return nil, errors.Join(errOnlineModelUnconfirmed, scope.Err())
	}
	return response.buffer.Bytes(), nil
}

type onlineModelOutput struct{ buffer bytes.Buffer }

func (out *onlineModelOutput) Write(chunk []byte) (int, error) {
	if len(chunk) > 8192-out.buffer.Len() {
		return 0, io.ErrShortBuffer
	}
	return out.buffer.Write(chunk)
}

func decodeOnlineModelReceipt(raw []byte, modelID string) (onlineModelReceipt, error) {
	var response struct {
		Status int `json:"http_status"`
		Body   struct {
			onlineModelReceipt
			Code string `json:"code"`
		} `json:"body"`
	}
	if len(raw) > 8192 || json.Unmarshal(raw, &response) != nil {
		return onlineModelReceipt{}, errOnlineModelUnconfirmed
	}
	if response.Status == http.StatusConflict && response.Body.Code == "model_in_use" {
		return onlineModelReceipt{}, errM3Active
	}
	if response.Status != http.StatusOK {
		return onlineModelReceipt{}, errOnlineModelUnavailable
	}
	body := response.Body.onlineModelReceipt
	if body.ModelID != modelID || body.ActiveChanged == nil || *body.ActiveChanged ||
		body.Detached != 2 || body.Deleted < 0 || body.Deleted > body.Detached {
		return onlineModelReceipt{}, errOnlineModelUnconfirmed
	}
	if body.Outcome == "removed" && body.Deleted == body.Detached && body.Cleanup == nil {
		return body, nil
	}
	// Some bytes may have been detached but not fully reclaimed; do not
	// classify as a confirmed removal or encourage automatic retry.
	return onlineModelReceipt{}, errOnlineModelUnconfirmed
}
