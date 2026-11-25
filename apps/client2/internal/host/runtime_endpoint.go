package host

import "respire-client2/internal/i18n"

import (
	"context"
	"crypto/rand"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os"
	"path/filepath"
)

// The caller holds takeover. Only the current listener and library locks matter.
func (m *Manager) archiveStoppedEndpoint(ctx context.Context, address string) error {
	path := filepath.Join(m.DataRoot, "runtime", "endpoint.json")
	original, err := os.Lstat(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	if !original.Mode().IsRegular() {
		return errors.New(i18n.Message3d218924060ab868)
	}
	installation, err := m.lockRuntimeDirectory(ctx, m.DataRoot)
	if err != nil {
		return fmt.Errorf(i18n.Message66d93f3cf33e2485, err)
	}
	defer installation.Close()

	profile := m.DataRoot
	config, err := os.ReadFile(filepath.Join(m.DataRoot, "client.json"))
	if err == nil {
		var selected struct {
			Directory string `json:"data_dir"`
		}
		if err := json.Unmarshal(config, &selected); err != nil {
			return errors.New(i18n.Message9cb0a1458d4b17b9)
		}
		if selected.Directory != "" {
			if !filepath.IsAbs(selected.Directory) {
				return errors.New(i18n.Messagef9089c09a6bb6fc9)
			}
			profile, err = filepath.EvalSymlinks(selected.Directory)
			if err != nil {
				return err
			}
			relative, err := filepath.Rel(m.DataRoot, profile)
			if (err != nil || !filepath.IsLocal(relative)) && !deviceConfigDefaultProfileLayout(m.DataRoot) {
				return errors.New(i18n.Message2331b7c0907bd175)
			}
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	if !samePath(profile, m.DataRoot) {
		account, err := m.lockRuntimeDirectory(ctx, profile)
		if err != nil {
			return fmt.Errorf(i18n.Message1f08c3a2e258450b, err)
		}
		defer account.Close()
	}
	if err := requireClosedRuntimePort(ctx, address); err != nil {
		return err
	}
	return archiveRuntimeRecord(path, original)
}

// Call with takeover and the affected library locks held.
func archiveRuntimeRecord(path string, original os.FileInfo) error {
	if original.IsDir() {
		return fmt.Errorf(i18n.Message7fe726524c66bc43, filepath.Base(path))
	}
	current, err := os.Lstat(path)
	if err != nil {
		return err
	}
	if !os.SameFile(current, original) || !current.ModTime().Equal(original.ModTime()) || current.Size() != original.Size() {
		return fmt.Errorf(i18n.Message00c6ac70b9adc0c6, filepath.Base(path))
	}
	var nonce [16]byte
	if _, err := rand.Read(nonce[:]); err != nil {
		return err
	}
	ext := filepath.Ext(path)
	archive := path[:len(path)-len(ext)] + ".stopped-" + hex.EncodeToString(nonce[:]) + ext
	if err := os.Rename(path, archive); err != nil {
		return fmt.Errorf(i18n.Messagee2618f2b2591bf00, filepath.Base(path), err)
	}
	return nil
}

func requireClosedRuntimePort(ctx context.Context, address string) error {
	connection, err := (&net.Dialer{}).DialContext(ctx, "tcp", address)
	if IsUnavailable(err) {
		return nil
	}
	if err != nil {
		return fmt.Errorf(i18n.Messagec2c98f403acd0dfc, address, err)
	}
	err = connection.Close()
	return errors.Join(fmt.Errorf(i18n.Messagea3727b6d5b134d26, address), err)
}
