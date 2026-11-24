//go:build darwin

package host

import "respire-client2/internal/i18n"

import (
	"context"
	"os/exec"
	"time"
)

// readSystemProxy asks SystemConfiguration for the active proxy settings.
// scutil ships with macOS and prints the same dictionary the network
// preference pane edits.
func readSystemProxy() systemProxy {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	out, err := exec.CommandContext(ctx, "/usr/sbin/scutil", "--proxy").Output()
	if err != nil {
		return systemProxy{Source: proxySourceUnavailable, Note: i18n.Message2cc9f69b045497e9}
	}
	return parseScutilProxy(string(out))
}
