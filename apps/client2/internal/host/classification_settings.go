//go:build linux || windows || (darwin && cgo)

package host

import "respire-client2/internal/i18n"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"os"
	"strings"
	"unicode"
)

type classificationSettings struct {
	Backend string `json:"backend"`
	Base    string `json:"api_base"`
	Model   string `json:"model"`
	HasKey  bool   `json:"has_key"`
}

func classificationSlot(store hostCredentialStore, label string) (*string, error) {
	value, err := store.readSlot("", "rsrs", "classify:"+label, true)
	if err == nil && value != nil && *value == "" {
		return nil, errors.New(i18n.Message5d30a215be45188d)
	}
	return value, err
}

func readClassificationSettings(store hostCredentialStore, backend, base string) (classificationSettings, string, error) {
	if backend == "" {
		selected, err := classificationSlot(store, "last-backend")
		if err != nil {
			return classificationSettings{}, "", err
		}
		backend = "jev"
		if selected != nil {
			backend = *selected
		}
	}
	settings := classificationSettings{Backend: backend}
	slot, keyEnv := "typesafe", "TYPESAFE_API_KEY"
	switch backend {
	case "jev":
		if base != "" {
			return settings, "", errors.New(i18n.Message24fecef5871c0c9f)
		}
		settings.Base, settings.Model = "https://api.typesafe.ai/v1/systemone", "jev-latest"
	case "ds":
		if base == "" {
			selected, err := classificationSlot(store, "ds-last-base")
			if err != nil {
				return settings, "", err
			}
			base = "https://api.deepseek.com/v1"
			if selected != nil {
				base = *selected
			}
		}
		base = strings.TrimRight(strings.TrimSpace(base), "/")
		parsed, err := url.Parse(base)
		if err != nil || parsed.Hostname() == "" || (parsed.Scheme != "http" && parsed.Scheme != "https") ||
			strings.ContainsAny(base, "@?#") || strings.IndexFunc(base, unicode.IsSpace) >= 0 {
			return settings, "", errors.New(i18n.Message6aef0eb7dbd52410)
		}
		host := providerHost(base)
		model, err := classificationSlot(store, "ds-model@"+host)
		if err != nil {
			return settings, "", err
		}
		settings.Base, settings.Model = base, "deepseek-flash"
		if model != nil {
			settings.Model = *model
		}
		slot, keyEnv = "ds@"+host, "DS_API_KEY"
	default:
		return settings, "", errors.New(i18n.Message13856ad6393da311)
	}
	key, err := classificationSlot(store, slot)
	if err != nil {
		return settings, "", err
	}
	settings.HasKey = key != nil || os.Getenv(keyEnv) != ""
	return settings, slot, nil
}

func (m *Manager) desktopClassificationConfig(ctx context.Context, args []string, secret string, save bool) (json.RawMessage, error) {
	var backend, base, model string
	for _, arg := range args {
		flag, value, _ := strings.Cut(arg, "=")
		switch flag {
		case "--backend":
			backend = value
		case "--api-base":
			if strings.TrimSpace(value) == "" {
				return nil, errors.New(i18n.Messagecba822bf038facd8)
			}
			base = value
		case "--model":
			model = strings.TrimSpace(value)
			if model == "" {
				return nil, errors.New(i18n.Message6a0b5342d69f950f)
			}
		default:
			return nil, errors.New(i18n.Message87030cf629eba0d3)
		}
	}
	if strings.ContainsAny(secret, "\r\n\x00") || len(secret) > 4096 || secret != "" && strings.TrimSpace(secret) == "" {
		return nil, errors.New(i18n.Message5a6ec1a6b5705541)
	}
	secret = strings.TrimSpace(secret)
	if save {
		takeover, err := m.lockRuntimeTakeover(ctx)
		if err != nil {
			return nil, err
		}
		defer takeover.Close()
	}
	store := nativeRuntimeCredentialStore(ctx)
	settings, slot, err := readClassificationSettings(store, backend, base)
	if err != nil {
		return nil, err
	}
	if save {
		if model != "" && settings.Backend != "ds" {
			return nil, errors.New(i18n.Message8fdbfb3b447f08de)
		}
		if err := saveDesktopClassification(store, settings, slot, model, secret); err != nil {
			return nil, err
		}
		if err := m.refreshDesktopClassificationCredentials(ctx, store); err != nil {
			return nil, fmt.Errorf(i18n.Message225787a9de8804dd, err)
		}
		settings, _, err = readClassificationSettings(store, settings.Backend, base)
		if err != nil {
			return nil, fmt.Errorf(i18n.Message162564f94bbeab05, err)
		}
	}
	return json.Marshal(map[string]any{"command": "classify-config", "status": "ok", "summary": settings})
}

func saveDesktopClassification(store hostCredentialStore, settings classificationSettings, slot, model, secret string) error {
	type change struct{ label, value string }
	var changes []change
	if secret != "" {
		changes = append(changes, change{slot, secret})
	}
	if settings.Backend == "ds" {
		changes = append(changes, change{"ds-last-base", settings.Base})
		if model != "" {
			changes = append(changes, change{"ds-model@" + providerHost(settings.Base), model})
		}
	}
	changes = append(changes, change{"last-backend", settings.Backend})
	for _, item := range changes {
		name := "classify:" + item.label
		previous, err := store.readSlot(runtimeLoginBackend(), "rsrs", name, true)
		if err != nil {
			return fmt.Errorf(i18n.Message06e6372cc8b5fab6, err)
		}
		if previous != nil && *previous == item.value {
			continue
		}
		if err := store.replace("rsrs", name, previous, item.value); err != nil {
			return fmt.Errorf(i18n.Message71c231dd93e6d40f, err)
		}
	}
	return nil
}

func (m *Manager) refreshDesktopClassificationCredentials(ctx context.Context, store hostCredentialStore) error {
	h, _, err := m.Health(ctx)
	if IsUnavailable(err) {
		// Credentials are saved in the keyring and the next start delivers them.
		return nil
	}
	if err != nil {
		return err
	}
	profile, err := m.coreConfigProfile()
	if err != nil {
		return err
	}
	if !samePath(h.DataDir, profile) || !samePath(h.InstallationRoot, m.DataRoot) ||
		!runtimeInstanceID.MatchString(h.InstanceID) || h.Generation == 0 || !validHash(h.AccountID) {
		return errors.New(i18n.Messageb072d03eadae626d)
	}
	installed, err := m.Installed()
	if err != nil {
		return err
	}
	if err := m.verifyInstalled(installed); err != nil {
		return err
	}
	if err := m.verifySelectedHealth(h, installed); err != nil {
		return err
	}
	if h.ImageSHA256 != installed.SHA256 {
		return errors.New(i18n.Message3f19e166f0e957bc)
	}
	return m.unlockVerifiedRuntime(ctx, h, store, "", true)
}
