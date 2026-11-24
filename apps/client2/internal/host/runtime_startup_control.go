package host

import (
	"bytes"
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"os/exec"
	"path/filepath"
	"reflect"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"time"
	"unicode/utf8"
)

const startupFrameLimit = 16384

type startupAcquire struct {
	SchemaVersion    uint32 `json:"schema_version"`
	InstallationRoot string `json:"installation_root"`
	Profile          string `json:"profile"`
	ControllerPID    uint32 `json:"controller_pid"`
	ControllerBirth  string `json:"controller_birth"`
}

type startupHeld struct {
	SchemaVersion    uint32 `json:"schema_version"`
	GuardID          string `json:"guard_id"`
	InstallationRoot string `json:"installation_root"`
	Profile          string `json:"profile"`
	AccountID        string `json:"account_id"`
	HelperPID        uint32 `json:"helper_pid"`
	HelperBirth      string `json:"helper_birth"`
}

type startupEndpoint struct {
	PID            uint32 `json:"pid"`
	ProcessStartID string `json:"process_start_id"`
	ImagePath      string `json:"image_path"`
	ImageSHA256    string `json:"image_sha256"`
	InstanceID     string `json:"instance_id"`
	AccountID      string `json:"account_id"`
	Generation     uint64 `json:"generation"`
	URL            string `json:"url"`
}

type startupFinish struct {
	GuardID  string           `json:"guard_id"`
	Endpoint *startupEndpoint `json:"endpoint"`
}

type startupReleased struct {
	SchemaVersion uint32           `json:"schema_version"`
	GuardID       string           `json:"guard_id"`
	Endpoint      *startupEndpoint `json:"endpoint"`
}

type runtimeStartup struct {
	ctx       context.Context
	cancel    context.CancelFunc
	workCtx   context.Context
	stopWork  context.CancelFunc
	cmd       *exec.Cmd
	input     io.WriteCloser
	output    io.ReadCloser
	held      startupHeld
	done      bool
	mu        sync.Mutex
	finishing bool
	exited    chan struct{}
	waitErr   error
}

func writeStartupFrame(output io.Writer, value any) error {
	raw, err := json.Marshal(value)
	if err != nil {
		return err
	}
	if len(raw) == 0 || len(raw) > startupFrameLimit {
		return errors.New("Invalid startup control frame size")
	}
	frame := binary.BigEndian.AppendUint32(nil, uint32(len(raw)))
	_, err = io.Copy(output, bytes.NewReader(append(frame, raw...)))
	return err
}

func readStartupFrame(input io.Reader, value any) error {
	var length uint32
	if err := binary.Read(input, binary.BigEndian, &length); err != nil {
		return err
	}
	if length == 0 || length > startupFrameLimit {
		return errors.New("Invalid startup control frame size")
	}
	raw := make([]byte, length)
	if _, err := io.ReadFull(input, raw); err != nil {
		return err
	}
	if !utf8.Valid(raw) {
		return errors.New("Invalid startup control UTF-8")
	}
	decoder := json.NewDecoder(bytes.NewReader(raw))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(value); err != nil {
		return err
	}
	var tail any
	if err := decoder.Decode(&tail); err != io.EOF {
		return errors.New("Unexpected startup control JSON suffix")
	}
	return nil
}

// Only absent suffixes are permitted; existing ancestors retain native identity checks.
func startupScopePath(path string) (string, error) {
	if !filepath.IsAbs(path) {
		return "", errors.New("Startup scope must be absolute")
	}
	for _, part := range strings.FieldsFunc(path, func(r rune) bool { return r == '/' || r == '\\' }) {
		if part == ".." {
			return "", errors.New("Startup scope contains parent traversal")
		}
	}
	path = filepath.Clean(filepath.FromSlash(path))
	volume := filepath.VolumeName(path)
	cursor := volume + string(filepath.Separator)
	parts := strings.Split(strings.TrimPrefix(path[len(volume):], string(filepath.Separator)), string(filepath.Separator))
	for index, part := range parts {
		if part == "" {
			continue
		}
		cursor = filepath.Join(cursor, part)
		info, err := os.Lstat(cursor)
		if errors.Is(err, os.ErrNotExist) {
			continue
		}
		if err != nil {
			return "", err
		}
		if modelUninstallRedirect(info) {
			if runtime.GOOS != "darwin" || index == len(parts)-1 || (cursor != "/var" && cursor != "/tmp" && cursor != "/etc") {
				return "", errors.New("Startup scope contains a symlink or reparse point")
			}
			target := "/private" + cursor
			for ancestor := target; ; ancestor = filepath.Dir(ancestor) {
				entry, err := os.Lstat(ancestor)
				if err != nil {
					return "", err
				}
				if !entry.IsDir() || modelUninstallRedirect(entry) {
					return "", errors.New("Startup system alias target was redirected")
				}
				if ancestor == filepath.Dir(ancestor) {
					break
				}
			}
			actual, err := canonicalSessionDirectory(cursor)
			if err != nil {
				return "", err
			}
			if actual != target {
				return "", errors.New("Startup system alias target changed")
			}
			cursor = target
		} else if index < len(parts)-1 && !info.IsDir() {
			return "", errors.New("Startup scope ancestor is not a directory")
		}
	}
	path = cursor
	ancestor := path
	for {
		canonical, err := canonicalSessionDirectory(ancestor)
		if err == nil {
			if !samePath(canonical, ancestor) {
				return "", errors.New("Startup scope was redirected")
			}
			rel, err := filepath.Rel(ancestor, filepath.Clean(path))
			if err != nil {
				return "", err
			}
			return filepath.Join(canonical, rel), nil
		}
		if !errors.Is(err, os.ErrNotExist) {
			return "", err
		}
		parent := filepath.Dir(ancestor)
		if parent == ancestor {
			return "", err
		}
		ancestor = parent
	}
}

// This projects the expected account only. Runtime validates unbound and migrated
// session claims and their receipts before issuing Held; Go grants no authority.
func startupProfileAccount(profile string) (string, error) {
	raw, err := readCredentialConfig(filepath.Join(profile, "session.json"), controlFrameLimit)
	if errors.Is(err, os.ErrNotExist) {
		return controlDigest([]byte(profile)), nil
	}
	if err != nil {
		return "", err
	}
	defer clear(raw)
	var fields map[string]json.RawMessage
	if json.Unmarshal(raw, &fields) != nil || fields == nil {
		return "", errors.New("Invalid startup session object")
	}
	if value, present := fields["user"]; present {
		var user *string
		if json.Unmarshal(value, &user) != nil || user == nil {
			return "", errors.New("Invalid startup session user")
		}
		if *user != "" {
			return controlDigest([]byte(*user)), nil
		}
	}
	return controlDigest([]byte(profile)), nil
}

func (m *Manager) startupScope() (root, profile, account string, err error) {
	root, err = startupScopePath(m.DataRoot)
	if err != nil {
		return
	}
	settings, err := readAgentSettingsObject(filepath.Join(root, "client.json"))
	if err != nil {
		return "", "", "", err
	}
	profile = root
	if value, present := settings["data_dir"]; present {
		directory, ok := value.(string)
		if !ok {
			return "", "", "", errors.New("Invalid selected startup profile")
		}
		if directory != "" {
			profile = directory
		}
	}
	profile, err = startupScopePath(profile)
	if err != nil {
		return
	}
	account, err = startupProfileAccount(profile)
	if err != nil {
		return "", "", "", err
	}
	return
}

func (m *Manager) acquireStartup(ctx context.Context, info InstalledRuntime) (guard *runtimeStartup, err error) {
	if err := m.verifyRuntimeSelection(info); err != nil {
		return nil, err
	}
	if _, ok := ctx.Deadline(); !ok {
		return nil, errors.New("Startup control requires a caller deadline")
	}
	if err := ctx.Err(); err != nil {
		return nil, err
	}
	root, profile, account, err := m.startupScope()
	if err != nil {
		return nil, err
	}
	_, port, err := m.runtimeAccountEndpoint()
	if err != nil {
		return nil, err
	}
	birth, err := hostControlBirth(ctx)
	if err != nil {
		return nil, err
	}
	// The helper owns its lock for at most 90 seconds after acquisition.
	// Start our shorter monotonic deadline before sending Acquire so a
	// delayed Held frame cannot authorize work after the helper has expired.
	childCtx, cancel := context.WithTimeout(ctx, 85*time.Second)
	image := filepath.Join(m.runtimeDir(info.Version), runtimeBinary())
	cmd := exec.CommandContext(childCtx, image, "--startup-control-internal")
	cmd.Dir = filepath.Dir(image)
	configureCommand(cmd)
	cmd.Env = runtimeServingCommand(image, root, "127.0.0.1", strconv.FormatUint(port, 10), "").Env
	cmd.Env = append(cmd.Env, "RSRS_DATA_DIR="+root, "RSRS_RPC_PORT="+strconv.FormatUint(port, 10))
	cmd.Stderr = os.Stderr
	input, err := cmd.StdinPipe()
	if err != nil {
		cancel()
		return nil, err
	}
	output, writer, err := os.Pipe()
	if err != nil {
		input.Close()
		cancel()
		return nil, err
	}
	cmd.Stdout = writer
	if err := cmd.Start(); err != nil {
		input.Close()
		output.Close()
		writer.Close()
		cancel()
		return nil, err
	}
	writer.Close()
	// Successful Finish cancels only the helper process. The serving handoff
	// still has to verify its receipt within the same conservative deadline.
	deadline, _ := childCtx.Deadline()
	workCtx, stopWork := context.WithDeadline(ctx, deadline)
	guard = &runtimeStartup{ctx: childCtx, cancel: cancel, workCtx: workCtx, stopWork: stopWork,
		cmd: cmd, input: input, output: output, exited: make(chan struct{})}
	child := guard
	go func() {
		child.waitErr = cmd.Wait()
		child.mu.Lock()
		if !child.finishing {
			child.stopWork()
		}
		child.mu.Unlock()
		close(child.exited)
	}()
	defer func() {
		if err != nil {
			err = errors.Join(fmt.Errorf("Runtime startup control: %w", err), childCtx.Err(), guard.Close())
			guard = nil
		}
	}()
	request := startupAcquire{1, root, profile, uint32(os.Getpid()), birth}
	if err = writeStartupFrame(input, request); err != nil {
		return
	}
	if err = readStartupFrame(output, &guard.held); err != nil {
		return
	}
	held := guard.held
	helperBirth, birthErr := runtimeControlProcessBirth(childCtx, cmd.Process.Pid)
	if birthErr != nil {
		return guard, birthErr
	}
	if held.SchemaVersion != 1 || !runtimeInstanceID.MatchString(held.GuardID) || held.GuardID[14] != '4' || !strings.ContainsRune("89ab", rune(held.GuardID[19])) ||
		held.HelperPID != uint32(cmd.Process.Pid) || helperBirth == "" || held.HelperBirth != helperBirth ||
		!samePath(held.InstallationRoot, root) || !samePath(held.Profile, profile) || held.AccountID != account {
		return guard, errors.New("Startup held receipt identity mismatch")
	}
	if err = verifyHostProcess(HealthInfo{PID: held.HelperPID, Exe: image}); err != nil {
		return
	}
	if err = guard.workCtx.Err(); err != nil {
		return
	}
	return guard, nil
}

func (g *runtimeStartup) Held() startupHeld { return g.held }

func (g *runtimeStartup) Finish(endpoint *startupEndpoint) (released startupReleased, err error) {
	if g.done {
		return released, errors.New("Startup control is already closed")
	}
	defer func() {
		if err != nil {
			err = errors.Join(err, g.ctx.Err(), g.Close())
			released = startupReleased{}
		}
	}()
	g.mu.Lock()
	g.finishing = true
	g.mu.Unlock()
	if err = g.workCtx.Err(); err != nil {
		return
	}
	if err = writeStartupFrame(g.input, startupFinish{g.held.GuardID, endpoint}); err != nil {
		return
	}
	if err = readStartupFrame(g.output, &released); err != nil {
		return
	}
	if released.SchemaVersion != 1 || released.GuardID != g.held.GuardID || !reflect.DeepEqual(released.Endpoint, endpoint) {
		return released, errors.New("Startup released receipt mismatch")
	}
	var tail [1]byte
	if n, readErr := g.output.Read(tail[:]); n != 0 || readErr != io.EOF {
		return released, errors.New("Unexpected data after startup released receipt")
	}
	<-g.exited
	err = g.waitErr
	g.done = true
	g.input.Close()
	g.output.Close()
	if err == nil {
		err = g.ctx.Err()
	}
	g.cancel()
	return
}

// Cancellation reaps only this owned helper; it never reports successful startup.
func (g *runtimeStartup) Close() error {
	defer g.stopWork()
	if g.done {
		return nil
	}
	g.done = true
	g.input.Close()
	killErr := g.cmd.Process.Kill()
	<-g.exited
	waitErr := g.waitErr
	g.output.Close()
	g.cancel()
	if g.cmd.ProcessState == nil {
		return fmt.Errorf("Startup helper exit unconfirmed: %w", errors.Join(killErr, waitErr))
	}
	return nil
}
