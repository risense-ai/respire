package host

import (
	"errors"
	"os"
	"path/filepath"

	"respire-client2/internal/i18n"
)

var errM3Active = errors.New(i18n.ModelUninstallActive)

func isM3Model(id string) bool {
	return id == "m3" || id == "bge-m3"
}

func m3AssetsUseTarget(paths []string, target string) (bool, error) {
	targetInfo, err := os.Lstat(target)
	if errors.Is(err, os.ErrNotExist) {
		return false, nil
	}
	if err != nil {
		return false, err
	}
	for _, path := range paths {
		path = windowsDOSPath(path)
		if !filepath.IsAbs(path) {
			return false, errors.New(i18n.Messageddf914c21920e538)
		}
		// Resolve aliases and compare directory identities, including short paths on Windows.
		resolved, err := filepath.EvalSymlinks(path)
		if err != nil {
			return false, errors.New(i18n.ModelUninstallAssetUnavailable)
		}
		for dir := filepath.Dir(resolved); ; dir = filepath.Dir(dir) {
			info, err := os.Stat(dir)
			if err != nil {
				return false, errors.New(i18n.ModelUninstallAssetUnavailable)
			}
			if os.SameFile(info, targetInfo) {
				return true, nil
			}
			if dir == filepath.Dir(dir) {
				break
			}
		}
	}
	return false, nil
}
