//go:build !windows

package host

func admitProfile(root, configured string) (profileAdmission, func(), error) {
	path, err := startupScopePath(root)
	return profileAdmission{root: path}, func() {}, err
}
