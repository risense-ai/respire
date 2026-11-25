//go:build !linux && !windows && (!darwin || !cgo)

package host

import "respire-client2/internal/i18n"

import (
	"context"
	"encoding/json"
	"errors"
)

func (m *Manager) desktopLegacyMigration(context.Context, string, []string, bool) (json.RawMessage, error) {
	return nil, errors.New(i18n.Messagee3d383f8cae2e709)
}
