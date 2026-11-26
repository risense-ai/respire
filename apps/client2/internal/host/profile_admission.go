package host

// A configured junction must still identify the directory admitted by New.
// Retain the native path handles for the duration of each sensitive operation.
type profileAdmission struct {
	root string
	reopen func() (func(), error)
}

func (m *Manager) holdProfile() (func(), error) {
	if m.profile.reopen == nil {
		return func() {}, nil
	}
	return m.profile.reopen()
}
