package desktop

import "respire-client2/internal/i18n"

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"time"

	"github.com/wailsapp/wails/v3/pkg/updater"
	"github.com/wailsapp/wails/v3/pkg/updater/providers/github"
	"respire-client2/internal/host"
)

func (d *desktop) initUpdater() error {
	config := github.Config{
		Repository: "risense-ai/respire-client2", ChecksumAsset: "SHA256SUMS", HTTPClient: d.m.HTTPClient,
		AssetMatcher: func(req updater.CheckRequest, assets []github.ReleaseAsset) int {
			for i, asset := range assets {
				if asset.Name == host.ClientAsset(req.Platform, req.Arch) {
					return i
				}
			}
			return -1
		},
	}
	stable, err := github.New(config)
	if err != nil {
		return err
	}
	config.Prerelease = true
	dev, err := github.New(config)
	if err != nil {
		return err
	}
	return d.app.Updater.Init(updater.Config{
		CurrentVersion: version,
		Providers:      []updater.Provider{newReleaseProvider(d.m.HTTPClient, d.m.Preferences, stable, dev)},
		Window:         updater.WindowNone,
	})
}

func (d *desktop) checkClient(download bool) error {
	if !d.updateMu.TryLock() {
		return errors.New(i18n.Message65cc0dd41b56ae4e)
	}
	defer d.updateMu.Unlock()
	if d.app.Updater.State() == updater.StateReady {
		return nil
	}
	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Minute)
	d.mu.Lock()
	d.updateCancel = cancel
	d.mu.Unlock()
	defer func() {
		d.mu.Lock()
		d.updateCancel = nil
		d.mu.Unlock()
		cancel()
	}()
	r, err := d.app.Updater.Check(ctx)
	if err == nil && r == nil {
		d.mu.Lock()
		d.clientVersion = ""
		d.mu.Unlock()
	}
	if err == nil && r != nil {
		d.mu.Lock()
		d.clientVersion, d.clientNotes, d.clientSource = r.Version, r.Notes, r.Channel
		d.mu.Unlock()
		if download {
			err = d.app.Updater.DownloadAndInstall(ctx)
			if err == nil && d.m.Preferences().ClientAutoRestart && d.canRestartClient() {
				err = d.app.Updater.Restart(ctx)
			}
		}
	}
	d.mu.Lock()
	d.clientError = ""
	if errors.Is(err, context.Canceled) {
		err = errors.New(i18n.Messageae5009c1eee90a0b)
	}
	if err != nil {
		d.clientError = err.Error()
	} else {
		d.clientNotes, d.clientSource = "", d.m.Preferences().ClientChannel
	}
	d.mu.Unlock()
	return err
}

// cancelUpdate unblocks a running Check/Download without replacing its lock owner.
func (d *desktop) cancelUpdate() error {
	d.mu.Lock()
	cancel := d.updateCancel
	d.mu.Unlock()
	if cancel == nil {
		return errors.New(i18n.Message832b98589fb131fd)
	}
	cancel()
	return nil
}

func (d *desktop) canRestartClient() bool {
	return exeDirWritable() && !d.m.HasActiveOperations() && !d.m.TaskSnapshot().Busy
}

// clientRestartBlocked explains why an in-place client update cannot restart
// right now; '' means allowed. The unwritable-directory case (per-machine
// installs under Program Files) defers to the setup.exe upgrade path.
func (d *desktop) clientRestartBlocked() error {
	if !exeDirWritable() {
		return errors.New(i18n.Message6484784d94a61324)
	}
	if d.m.HasActiveOperations() || d.m.TaskSnapshot().Busy {
		return errors.New(i18n.Message5d9c55169fce1f9f)
	}
	return nil
}

// exeDirWritable probes whether the running executable's directory accepts
// writes via a temporary file. Var-backed so tests can stub it.
var exeDirWritable = func() bool {
	exe, err := os.Executable()
	if err != nil {
		return false
	}
	return dirWritable(filepath.Dir(exe))
}

// dirWritable reports whether dir accepts a create-and-delete probe.
func dirWritable(dir string) bool {
	probe, err := os.CreateTemp(dir, ".respire-update-probe-*")
	if err != nil {
		return false
	}
	name := probe.Name()
	if err := probe.Close(); err != nil {
		os.Remove(name)
		return false
	}
	os.Remove(name)
	return true
}

func (d *desktop) savePreferences(p host.Preferences) error {
	if p.ClientChannel != d.m.Preferences().ClientChannel {
		if !d.updateMu.TryLock() {
			return errors.New(i18n.Messagee405d43f871a6e30)
		}
		defer d.updateMu.Unlock()
		if d.app.Updater.State() == updater.StateReady {
			return errors.New(i18n.Message21f9ddbb91524a1b)
		}
	}
	if err := d.m.SavePreferences(p); err != nil {
		return err
	}
	d.applyAppearance()
	return nil
}
