package host

import "respire-client2/internal/i18n"

import (
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
)

// CLI language is a host preference; the CLI exposes no language RPC command.
func (m *Manager) SetCLILanguage(language string) error {
	if language != "zh" && language != "en" {
		return errors.New(i18n.Message65d97b2e2cf00c4a)
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	path := filepath.Join(filepath.Dir(m.Root), "client.json")
	settings := map[string]json.RawMessage{}
	b, err := os.ReadFile(path)
	if err != nil && !errors.Is(err, os.ErrNotExist) {
		return err
	}
	if err == nil {
		if err = json.Unmarshal(b, &settings); err != nil {
			return err
		}
		if settings == nil {
			return errors.New(i18n.Messagec7fa9c9b811ffdb6)
		}
	}
	settings["lang"], err = json.Marshal(language)
	if err != nil {
		return err
	}
	return writeJSON(path, settings)
}
