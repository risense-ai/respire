package host

import "respire-client2/internal/i18n"

import (
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
