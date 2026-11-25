package host

import (
	"context"
	"fmt"
	"net/http"
	"os"
	"path/filepath"
	"time"
)

// Use capability negotiation, not a version lock, and do not probe HTTP
// unless the selected Runtime image and exact local endpoint were verified.
func (m *Manager) runtimeModelInstallCapability(ctx context.Context) (HealthInfo, bool, error) {
	if m.startupControl == nil {
		return HealthInfo{}, false, nil
	}
	selected, err := m.Installed()
	if os.IsNotExist(err) {
		// No selected Runtime has been installed yet: use the old bootstrap.
		return HealthInfo{}, false, nil
	}
	if err != nil {
		return HealthInfo{}, false, err
	}
	if err = m.verifyRuntimeSelection(selected); err != nil {
		return HealthInfo{}, false, err
	}
	address, _, err := m.runtimeAccountEndpoint()
	if err != nil {
		return HealthInfo{}, false, err
	}
	probe, cancel := context.WithTimeout(ctx, 3*time.Second)
	defer cancel()
	health, _, err := m.Health(probe)
	if err != nil {
		return HealthInfo{}, false, err
	}
	if !health.StartupComplete {
		return HealthInfo{}, false, errOnlineModelUnconfirmed
	}
	if err = m.verifySelectedHealth(health, selected); err != nil {
		return HealthInfo{}, false, err
	}
	req, err := http.NewRequestWithContext(probe, http.MethodHead, "http://"+address+"/api/runtime/private-model/install", nil)
	if err != nil {
		return HealthInfo{}, false, err
	}
	res, err := m.localClient.Do(req)
	if err != nil {
		return HealthInfo{}, false, err
	}
	defer res.Body.Close()
	supported, err := runtimeInstallCapabilityResult(res.StatusCode, res.Header.Get("X-Respire-Native-Model-Install"), health.StoreReady)
	return health, supported, err
}

func runtimeInstallCapabilityResult(status int, marker string, ready bool) (bool, error) {
	switch status {
	case http.StatusNotFound, http.StatusMethodNotAllowed:
		return false, nil // A verified, running older Runtime.
	case http.StatusNoContent:
		if ready && marker == "pinned-pair-v1" {
			return true, nil
		}
	}
	return false, errOnlineModelUnconfirmed
}

// These are Runtime-derived fixed locations, not paths from a desktop
// request. Size is a presence check; activation still verifies exact hashes.
func (m *Manager) managedM3Pair(health HealthInfo) (weights, tokenizer string, ready bool) {
	if !samePath(m.DataRoot, health.InstallationRoot) || !filepath.IsAbs(health.DataDir) {
		return "", "", false
	}
	root, err := canonicalSessionDirectory(m.DataRoot)
	if err != nil {
		return "", "", false
	}
	account, err := canonicalSessionDirectory(health.DataDir)
	if err != nil {
		return "", "", false
	}
	base := filepath.Join(root, "runtime", "model-assets-v1", controlDigest([]byte(account)))
	dir := filepath.Join(base, "m3")
	for _, path := range []string{filepath.Join(base, ".ownership", "lock.db"), dir} {
		if err := checkModelUninstallPath(windowsDOSPath(path)); err != nil {
			return "", "", false
		}
	}
	weights, tokenizer = filepath.Join(dir, "model_quantized.onnx"), filepath.Join(dir, "tokenizer.json")
	ready = regularManagedModelAsset(weights, modelFileSpecs["quantized"].size) &&
		regularManagedModelAsset(tokenizer, modelFileSpecs["tokenizer"].size)
	return
}

func regularManagedModelAsset(path string, size int64) bool {
	info, err := os.Lstat(path)
	return err == nil && info.Mode().IsRegular() && !modelUninstallRedirect(info) && info.Size() == size
}

func (m *Manager) downloadManagedM3(ctx context.Context, health HealthInfo, definition modelDefinition, origins []string) error {
	if weights, tokenizer, installed := m.managedM3Pair(health); installed {
		if modelFileValid(weights, modelFileSpecs["quantized"]) && modelFileValid(tokenizer, modelFileSpecs["tokenizer"]) {
			return nil
		}
		// Do not overwrite or silently bless a same-size invalid pair.
		return errOnlineModelUnconfirmed
	}
	root, err := os.MkdirTemp("", "respire-model-download-")
	if err != nil {
		return err
	}
	// root was created by this call; no user or server supplied path is used.
	defer os.RemoveAll(root)
	weightsSpec, tokenizerSpec := modelFileSpecs["quantized"], modelFileSpecs["tokenizer"]
	if err := m.downloadModelFiles(ctx, definition, root, []modelFileSpec{weightsSpec, tokenizerSpec}, origins); err != nil {
		return err
	}
	weights, err := os.Open(filepath.Join(root, filepath.FromSlash(weightsSpec.rel)))
	if err != nil {
		return err
	}
	defer weights.Close()
	tokenizer, err := os.Open(filepath.Join(root, filepath.FromSlash(tokenizerSpec.rel)))
	if err != nil {
		return err
	}
	defer tokenizer.Close()
	if err := m.installRuntimeOwnedModel(ctx, health, definition.ID, weights, tokenizer); err != nil {
		return fmt.Errorf("publish downloaded model to verified Runtime: %w", err)
	}
	return nil
}
