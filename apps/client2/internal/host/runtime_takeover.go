package host

import (
	"context"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"time"

	"respire-client2/internal/i18n"
)

// Runtime must use the same exclusion as the CLI takeover transaction.
func (m *Manager) lockRuntimeTakeover(ctx context.Context) (io.Closer, error) {
	root, err := filepath.EvalSymlinks(m.DataRoot)
	if err != nil {
		return nil, err
	}
	if !samePath(root, m.DataRoot) {
		return nil, errors.New(i18n.Message1dd82294144bcad1)
	}
	directory := root
	for _, component := range []string{"runtime", "takeover"} {
		directory = filepath.Join(directory, component)
		info, err := os.Lstat(directory)
		if errors.Is(err, os.ErrNotExist) {
			continue // Only the authority may create the lock directory.
		}
		if err != nil {
			return nil, err
		}
		if !info.IsDir() || modelUninstallRedirect(info) {
			return nil, errors.New(i18n.Messagecd52450b2702ac40)
		}
	}
	var wait time.Duration
	if deadline, present := ctx.Deadline(); present {
		wait = time.Until(deadline)
	}
	return m.acquireRuntimeOwnership(ctx, runtimeTakeoverOwnership, directory, wait)
}

func (m *Manager) lockRuntimeDirectory(ctx context.Context, directory string) (io.Closer, error) {
	return m.acquireRuntimeOwnership(ctx, runtimeDirectoryOwnership, directory, 0)
}

func (m *Manager) acquireRuntimeOwnership(ctx context.Context, kind runtimeOwnershipKind, directory string, wait time.Duration) (io.Closer, error) {
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	if m.hostAuthority == nil {
		return nil, errRuntimeHostAuthorityUnavailable
	}
	path := filepath.Join(directory, "lock.db")
	if info, err := os.Lstat(path); err == nil {
		if !info.Mode().IsRegular() || modelUninstallRedirect(info) {
			return nil, errors.New(i18n.Message42f6332cae24e5bf)
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	lock, err := m.hostAuthority.acquire(ctx, kind, m.DataRoot, directory, wait)
	if err != nil {
		return nil, fmt.Errorf(i18n.Message647440dbb8d37599, err)
	}
	if lock == nil {
		return nil, errors.New("Runtime host authority returned no ownership")
	}
	if err := ctx.Err(); err != nil {
		return nil, errors.Join(err, lock.Close())
	}
	return lock, nil
}
