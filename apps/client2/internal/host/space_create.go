package host

import "respire-client2/internal/i18n"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"strings"
)

func (m *Manager) desktopSpaceCreate(ctx context.Context, name string) (json.RawMessage, error) {
	name = strings.TrimSpace(name)
	if !desktopSpaceName.MatchString(name) || name == "main" {
		return nil, errors.New(i18n.Messageb9e8949ef63203f6)
	}
	profile := filepath.Join(m.DataRoot, "accounts", name)
	return m.changeDeviceConfig(ctx, deviceConfigIntent{DataDir: &profile}, nil, configTargetNewSpace)
}

type freshSpaceDirectory struct {
	path     string
	original os.FileInfo
}

// The caller holds takeover. Mkdir is the only admission for a new space.
func createFreshSpaceDirectory(path string) (*freshSpaceDirectory, error) {
	parent := filepath.Dir(path)
	if err := os.Mkdir(parent, 0700); err != nil && !errors.Is(err, os.ErrExist) {
		return nil, err
	}
	if err := checkRuntimeAccountDirectory(parent); err != nil {
		return nil, err
	}
	if err := os.Mkdir(path, 0700); err != nil {
		if errors.Is(err, os.ErrExist) {
			return nil, fmt.Errorf(i18n.Messagef86e78e08c8a6738, filepath.Base(path))
		}
		return nil, err
	}
	info, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	space := &freshSpaceDirectory{path, info}
	if err := space.verifyEmpty(); err != nil {
		return nil, err
	}
	return space, nil
}

func (s freshSpaceDirectory) verifyIdentity() error {
	if err := checkRuntimeAccountDirectory(s.path); err != nil {
		return err
	}
	info, err := os.Lstat(s.path)
	if err != nil || !os.SameFile(s.original, info) {
		return errors.Join(errors.New(i18n.Message7126a491bea2d881), err)
	}
	return nil
}

func (s freshSpaceDirectory) verifyEmpty() error {
	if err := s.verifyIdentity(); err != nil {
		return err
	}
	entries, err := os.ReadDir(s.path)
	if err != nil {
		return err
	}
	for _, entry := range entries {
		if entry.Name() != "lock.db" {
			return errors.New(i18n.Message2f57c438fd8ba02f)
		}
		if _, err := profileFile(filepath.Join(s.path, entry.Name())); err != nil {
			return err
		}
	}
	return nil
}

func (s freshSpaceDirectory) removeEmpty() error {
	if err := s.verifyEmpty(); err != nil {
		return err
	}
	// Remove refuses a nonempty directory, including one with a Runtime lock file.
	return os.Remove(s.path)
}

func (s freshSpaceDirectory) publishOwner() error {
	if err := s.verifyEmpty(); err != nil {
		return err
	}
	data, err := json.Marshal(map[string]string{"name": filepath.Base(s.path)})
	if err != nil {
		return err
	}
	return writeRuntimeAccountRecord(filepath.Join(s.path, "space_owner.json"), data, nil)
}

func (s freshSpaceDirectory) result() (json.RawMessage, error) {
	name := filepath.Base(s.path)
	const hint = i18n.Messagedb3f9c22d802ce7f
	return json.Marshal(map[string]any{
		"command": "space", "status": "ok",
		"summary": map[string]string{"action": "create", "name": name},
		"items":   []map[string]string{{"name": "space", "status": "ok", "value": name}},
		"details": map[string]string{"name": name, "dir": s.path, "hint": hint},
		"actions": []string{hint},
	})
}
