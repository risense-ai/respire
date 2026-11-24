package host

import "respire-client2/internal/i18n"

import (
	"encoding/json"
	"errors"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"strings"
)

// DeviceServerAddress can be read while the library is locked or Runtime is offline.
func (m *Manager) DeviceServerAddress() (string, error) {
	const defaultServer = "https://api.rsrs.rs"
	raw, err := readCredentialConfig(filepath.Join(m.DataRoot, "client.json"), 1<<20)
	if errors.Is(err, os.ErrNotExist) {
		return defaultServer, nil
	}
	if err != nil {
		return "", fmt.Errorf(i18n.Message874e389cb50b756c, err)
	}
	var fields map[string]json.RawMessage
	if err := json.Unmarshal(raw, &fields); err != nil || fields == nil {
		return "", errors.New(i18n.Message8ebfeebbeed7f9f3)
	}
	field, present := fields["addr"]
	if !present {
		return defaultServer, nil
	}
	var address *string
	if err := json.Unmarshal(field, &address); err != nil || address == nil {
		return "", errors.New(i18n.Message7ecccf3010c55eb4)
	}
	value := strings.TrimSpace(*address)
	if value == "" {
		return defaultServer, nil
	}
	parsed, err := url.Parse(value)
	if err != nil || parsed.Hostname() == "" || (parsed.Scheme != "http" && parsed.Scheme != "https") ||
		parsed.User != nil || parsed.Fragment != "" {
		return "", errors.New(i18n.Message1da83441376c8110)
	}
	return value, nil
}
