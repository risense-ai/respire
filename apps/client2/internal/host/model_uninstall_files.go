package host

import "respire-client2/internal/i18n"

import (
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
	"unicode/utf8"
)

func checkModelUninstallPath(path string) error {
	path = windowsDOSPath(path)
	if !filepath.IsAbs(path) || !utf8.ValidString(path) || filepath.Clean(path) != path {
		return fmt.Errorf(i18n.Message5d4b1260d618c7be, path)
	}
	for part := path; ; part = filepath.Dir(part) {
		parent := filepath.Dir(part)
		if part != parent {
			name := filepath.Base(part)
			if strings.ContainsAny(name, ":\\") || strings.HasSuffix(name, ".") || strings.HasSuffix(name, " ") {
				return fmt.Errorf(i18n.Message15c7088f974d7ee4, part)
			}
		}
		info, err := os.Lstat(part)
		switch {
		case err == nil:
			if modelUninstallRedirect(info) || (part != path && !info.IsDir()) {
				return fmt.Errorf(i18n.Messageec12441c53feeeb3, part)
			}
		case !errors.Is(err, os.ErrNotExist):
			return err
		}
		if part == parent {
			return nil
		}
	}
}

func modelUninstallFile(path string) ([]byte, error) {
	if err := checkModelUninstallPath(path); err != nil {
		return nil, err
	}
	raw, err := readCredentialConfig(path, 64<<10)
	if errors.Is(err, os.ErrNotExist) {
		return nil, nil
	}
	return raw, err
}

func modelUninstallObject(path string) ([]byte, error) {
	raw, err := modelUninstallFile(path)
	if err != nil || raw == nil {
		return raw, err
	}
	var value map[string]json.RawMessage
	if err := json.Unmarshal(raw, &value); err != nil || value == nil {
		return nil, fmt.Errorf(i18n.Message15e1aae105b4c3a4, path)
	}
	return raw, nil
}

func modelUninstallProfile(root string, config []byte) (string, error) {
	var device struct {
		DataDir string `json:"data_dir"`
	}
	if config != nil {
		if err := json.Unmarshal(config, &device); err != nil {
			return "", err
		}
	}
	profile := root
	if device.DataDir != "" {
		var err error
		profile, err = expandTilde(device.DataDir)
		if err != nil {
			return "", err
		}
	}
	profile = windowsDOSPath(profile)
	if err := checkModelUninstallPath(profile); err != nil {
		return "", err
	}
	relative, err := filepath.Rel(root, profile)
	if err != nil || !filepath.IsLocal(relative) {
		return "", errors.New(i18n.Message7075916ca81c49da)
	}
	stat, err := os.Stat(profile)
	if err != nil {
		return "", err
	}
	if !stat.IsDir() {
		return "", errors.New(i18n.Messagee64e48bc7bcdc9ff)
	}
	return profile, nil
}

func checkM3UninstallTarget(target, profile string) error {
	for _, key := range []string{"RSRS_M3_DIR", "ONEMEMORY_M3_DIR", "RESPIRE_M3_DIR"} {
		if raw, ok := os.LookupEnv(key); ok {
			expanded, err := expandTilde(strings.TrimSpace(raw))
			if err != nil {
				return err
			}
			if err := checkModelUninstallPath(expanded); err != nil {
				return err
			}
			if !samePath(expanded, target) {
				return fmt.Errorf(i18n.Message52e23d86fa21502a, key)
			}
			break
		}
	}
	relative, err := filepath.Rel(target, profile)
	if err == nil && filepath.IsLocal(relative) {
		return errors.New(i18n.Message2169602bfcaa7deb)
	}
	if strings.Contains(filepath.ToSlash(target), "/usr/lib/respire/models") {
		return errors.New(i18n.Messageefd0f1527c2d5aa7)
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return err
	}
	home, err = filepath.EvalSymlinks(home)
	if err != nil {
		return err
	}
	if relative, err := filepath.Rel(target, windowsDOSPath(home)); err == nil && filepath.IsLocal(relative) {
		return errors.New(i18n.Message691a2b3256b5df8f)
	}
	return checkModelUninstallPath(target)
}
