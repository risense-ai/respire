package host

import (
	"errors"
	"path/filepath"
)

var errProfileScope = errors.New("configured profile has no trusted local directory identity")

// A configured junction must still identify the directory admitted by New.
// Retain the native path handles for the duration of each sensitive operation.
type profileAdmission struct {
	root          string
	reopen        func() (func(), error)
	closeIdentity func()
}

func (m *Manager) holdProfile() (func(), error) {
	if m.profile.reopen == nil {
		return func() {}, nil
	}
	if !samePath(m.DataRoot, m.profile.root) || !samePath(m.Root, filepath.Join(m.profile.root, "client2")) {
		return nil, errProfileScope
	}
	return m.profile.reopen()
}


func (p profileAdmission) close() {
	if p.closeIdentity != nil {
		p.closeIdentity()
	}
}
