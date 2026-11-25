package host

import "respire-client2/internal/i18n"

import (
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"strings"
)

type ExternalRuntime struct {
	Version string `json:"version"`
	Path    string `json:"path"`
}

// npm and manual installations share the data-root bin directory but are not client-managed.
func (m *Manager) External(ctx context.Context) (ExternalRuntime, error) {
	// The external host executable is the CLI; only the managed runtime
	// switched to the split rsrs-runtime binary.
	path := filepath.Join(filepath.Dir(m.Root), "bin", cliBinary())
	if _, err := os.Stat(path); err != nil {
		return ExternalRuntime{}, err
	}
	if err := verifyExecutable(path); err != nil {
		return ExternalRuntime{}, err
	}
	cmd := exec.CommandContext(ctx, path, "--version")
	configureCommand(cmd)
	out, err := cmd.CombinedOutput()
	if err != nil {
		return ExternalRuntime{}, fmt.Errorf(i18n.Message902f9b59fc998a26, err, strings.TrimSpace(string(out)))
	}
	fields := strings.Fields(string(out))
	if len(fields) < 2 || fields[0] != "rsrs" || !validVersion(fields[1]) {
		return ExternalRuntime{}, fmt.Errorf(i18n.Message67b0609a6195e590, strings.TrimSpace(string(out)))
	}
	return ExternalRuntime{Version: fields[1], Path: path}, nil
}
