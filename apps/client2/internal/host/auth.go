package host

import "respire-client2/internal/i18n"

import (
	"encoding/json"
	"errors"
	"strings"
)

// Nonsecret routing fields are allowlisted by commandSpec; credentials never become argv.
func authParams(args []string, secret string) (map[string]string, error) {
	params := map[string]string{}
	if secret != "" {
		if len(secret) > 49152 {
			return nil, errors.New(i18n.Message5aa98873ffeca9ba)
		}
		if err := json.Unmarshal([]byte(secret), &params); err != nil {
			return nil, errors.New(i18n.Messaged2697f1f8b38c40f)
		}
		if params == nil {
			return nil, errors.New(i18n.Messaged3ad0f39cb4f14ae)
		}
	}
	for key, value := range params {
		switch key {
		case "password", "super", "secret", "kdf_salt", "wrapped_urk", "urk_nonce", "totp":
		default:
			return nil, errors.New(i18n.Message042cd4da5ba0d5a5)
		}
		if len(value) > 16384 || strings.ContainsRune(value, 0) {
			return nil, errors.New(i18n.Message4df21b707cc5d482)
		}
	}
	for _, arg := range args {
		key, value, ok := strings.Cut(strings.TrimPrefix(arg, "--"), "=")
		if !ok || (key != "user" && key != "addr") {
			return nil, errors.New(i18n.Message31382f79fbffa059)
		}
		if len(value) > 16384 || strings.ContainsRune(value, 0) {
			return nil, errors.New(i18n.Messagee0aa34f8b1ccb4e5)
		}
		params[key] = value
	}
	return params, nil
}
