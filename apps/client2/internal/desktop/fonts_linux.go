package desktop

import "respire-client2/internal/i18n"

import (
	"context"
	"fmt"
	"os/exec"
	"strings"
	"time"
)

func platformFonts() ([]string, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()
	out, err := exec.CommandContext(ctx, "fc-list", ":", "family").Output()
	if err != nil {
		return nil, fmt.Errorf(i18n.Message91695c0e2f11ac4b, err)
	}
	return strings.FieldsFunc(string(out), func(c rune) bool { return c == ',' || c == '\n' }), nil
}
