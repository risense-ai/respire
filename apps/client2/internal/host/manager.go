package host

import "respire-client2/internal/i18n"

import (
	"context"
	"crypto/rand"
	"encoding/json"
	"errors"
	"fmt"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"
)

type Preferences struct {
	AutoRuntime    bool   `json:"autoRuntime"`
	AutoUpdate     bool   `json:"autoUpdate"`
	RuntimeChannel string `json:"runtimeChannel"`
	ClientChannel  string `json:"clientChannel"`
	Layout         string `json:"layout"`
	Theme          string `json:"theme"`
	Language       string `json:"language"`
	ModelMirror    string `json:"modelMirror"`
	// ProxyMode is "system" (environment variables, then the OS proxy) or
	// "direct" (never proxy). Empty in older files and migrated to system.
	ProxyMode string `json:"proxyMode"`
	// Per-component update settings; AutoUpdate above is the legacy master
	// switch kept only to migrate older preference files.
	ClientUpdateInterval  string `json:"clientUpdateInterval"`
	RuntimeUpdateInterval string `json:"runtimeUpdateInterval"`
	ClientAutoUpdate      bool   `json:"clientAutoUpdate"`
	ClientAutoRestart     bool   `json:"clientAutoRestart"`
	RuntimeAutoUpdate     bool   `json:"runtimeAutoUpdate"`
	RuntimeAutoRestart    bool   `json:"runtimeAutoRestart"`
	SidebarWidth          int    `json:"sidebarWidth"`
	MemoryIndexWidth      int    `json:"memoryIndexWidth"`
	// Versioned first-run wizard state; steps and sync intent only — never
	// credentials or recovery material.
	OnboardingVersion    int    `json:"onboardingVersion"`
	OnboardingCompleted  bool   `json:"onboardingCompleted"`
	OnboardingStep       string `json:"onboardingStep"`
	OnboardingSyncChoice string `json:"onboardingSyncChoice"`
}

// updateIntervalValues are the cadences the updates page offers.
var updateIntervalValues = map[string]bool{
	"5m": true, "30m": true, "1h": true, "6h": true, "24h": true,
}

// onboardingSteps is the wizard's step enum; "done" is the terminal step.
var onboardingSteps = map[string]bool{
	"language": true, "sync": true, "account": true, "recovery": true,
	"model": true, "device": true, "confirm": true, "execute": true,
	"agents": true, "import": true, "done": true,
}

// onboardingSyncChoices records the cloud/offline intent picked in the
// wizard; empty means the step has not been answered yet.
var onboardingSyncChoices = map[string]bool{
	"": true, "cloud": true, "offline": true,
}

func NormalizeModelMirror(raw string) (string, error) {
	mirror := strings.TrimSpace(raw)
	if mirror == "" || mirror == "auto" {
		return "auto", nil
	}
	mirror = strings.TrimRight(mirror, "/")
	for _, origin := range modelMirrors {
		_, host, _ := strings.Cut(origin, "://")
		if strings.EqualFold(mirror, host) {
			return origin, nil
		}
	}
	parsed, err := url.Parse(mirror)
	if err != nil || (parsed.Scheme != "http" && parsed.Scheme != "https") || parsed.Hostname() == "" ||
		parsed.User != nil || parsed.ForceQuery || parsed.RawQuery != "" || strings.Contains(mirror, "#") ||
		(parsed.Path != "" && parsed.Path != "/") || strings.HasSuffix(parsed.Host, ":") {
		return "", errors.New("model mirror must be auto or an HTTP(S) origin")
	}
	if port := parsed.Port(); port != "" {
		number, err := strconv.Atoi(port)
		if err != nil || number < 1 || number > 65535 {
			return "", errors.New("model mirror must use a valid TCP port")
		}
	}
	for _, origin := range modelMirrors {
		_, host, _ := strings.Cut(origin, "://")
		if parsed.Scheme == "https" && strings.EqualFold(parsed.Host, host) {
			return origin, nil
		}
	}
	return strings.ToLower(parsed.Scheme) + "://" + parsed.Host, nil
}

// defaultPreferences is the state of a brand-new installation: the wizard
// has not run yet, so onboardingCompleted stays false.
func defaultPreferences() Preferences {
	// RuntimeChannel starts on dev until the runtime stable train opens:
	// stable promotion is gated on G1 acceptance (respire-runtime
	// docs/version-contract.md), and /releases/latest answers 404 while the
	// repository has no full release. Flip back to "stable" once the first
	// stable runtime is published.
	return Preferences{
		AutoRuntime: true, AutoUpdate: true, RuntimeChannel: "dev", ClientChannel: "stable", Layout: "sidebar", Theme: "system", Language: "",
		ProxyMode:            ProxyModeSystem,
		ModelMirror:          "auto",
		ClientUpdateInterval: "6h", RuntimeUpdateInterval: "6h", ClientAutoUpdate: true, RuntimeAutoUpdate: true,
		ClientAutoRestart: true, RuntimeAutoRestart: true,
		OnboardingVersion:   1,
		OnboardingCompleted: false,
		OnboardingStep:      "language",
	}
}

// migratePreferences fills settings that older preference files predate: the
// per-component auto-update switches inherit the legacy master switch, and
// unset intervals take the historical six-hour cadence.
func migratePreferences(p Preferences, stored map[string]json.RawMessage) Preferences {
	return MigratePreferences(p, stored)
}

// MigratePreferences is the exported form used when importing preference
// backups produced by older builds.
func MigratePreferences(p Preferences, stored map[string]json.RawMessage) Preferences {
	if _, ok := stored["clientAutoUpdate"]; !ok {
		p.ClientAutoUpdate = p.AutoUpdate
	}
	if _, ok := stored["clientAutoRestart"]; !ok {
		p.ClientAutoRestart = true
	}
	if _, ok := stored["runtimeAutoUpdate"]; !ok {
		p.RuntimeAutoUpdate = p.AutoUpdate
	}
	if _, ok := stored["runtimeAutoRestart"]; !ok {
		p.RuntimeAutoRestart = true
	}
	// Preference files written before the wizard existed belong to existing
	// users: they must not be forced through onboarding again. Only files
	// that already carry the wizard state keep it.
	if _, ok := stored["onboardingCompleted"]; !ok {
		p.OnboardingVersion = 1
		p.OnboardingCompleted = true
		p.OnboardingStep = "done"
	}
	if p.OnboardingVersion < 1 {
		p.OnboardingVersion = 1
	}
	if p.OnboardingCompleted {
		p.OnboardingStep = "done"
	}
	if !onboardingSteps[p.OnboardingStep] {
		p.OnboardingStep = "done"
	}
	if !onboardingSyncChoices[p.OnboardingSyncChoice] {
		p.OnboardingSyncChoice = ""
	}
	if !updateIntervalValues[p.ClientUpdateInterval] {
		p.ClientUpdateInterval = "6h"
	}
	if !updateIntervalValues[p.RuntimeUpdateInterval] {
		p.RuntimeUpdateInterval = "6h"
	}
	// Preference files written before proxy support keep the previous
	// behaviour available: an unset mode means "follow the environment and
	// the system proxy".
	if p.ProxyMode == "" {
		p.ProxyMode = ProxyModeSystem
	}
	mirror, err := NormalizeModelMirror(p.ModelMirror)
	if err != nil {
		mirror = "auto"
	}
	p.ModelMirror = mirror
	if p.SidebarWidth < 0 {
		p.SidebarWidth = 0
	}
	if p.MemoryIndexWidth < 0 {
		p.MemoryIndexWidth = 0
	}
	return p
}

func (p Preferences) validate() error {
	if p.Language != "" && p.Language != "en" && p.Language != "zh" {
		return errors.New("language must be en or zh")
	}
	if p.Theme != "system" && p.Theme != "light" && p.Theme != "dark" {
		return errors.New(i18n.Message4109a320f89efdbc)
	}
	if p.Layout != "sidebar" && p.Layout != "compact" {
		return errors.New(i18n.Message466a6c0577daccd0)
	}
	if !proxyModes[p.ProxyMode] {
		return errors.New(i18n.Message845bac57ec2916e1)
	}
	if _, err := NormalizeModelMirror(p.ModelMirror); err != nil {
		return err
	}
	for _, channel := range []string{p.RuntimeChannel, p.ClientChannel} {
		if channel != "stable" && channel != "dev" {
			return errors.New(i18n.Message6ebb4f61190f144d)
		}
	}
	if !updateIntervalValues[p.ClientUpdateInterval] || !updateIntervalValues[p.RuntimeUpdateInterval] {
		return errors.New(i18n.Messagedfb39286765551ad)
	}
	if p.SidebarWidth < 0 {
		return errors.New(i18n.Message4800fd5dd09114fb)
	}
	if p.MemoryIndexWidth < 0 {
		return errors.New(i18n.Message5095729ba5e87cfd)
	}
	if p.OnboardingVersion != 1 {
		return errors.New(i18n.Message54f8b0f3425b7b84)
	}
	if !onboardingSteps[p.OnboardingStep] {
		return errors.New(i18n.Messagec61a472306b5b44f)
	}
	if !onboardingSyncChoices[p.OnboardingSyncChoice] {
		return errors.New(i18n.Messageb4ee4252a554c0a1)
	}
	return nil
}

type TaskState struct {
	ID    string `json:"id,omitempty"`
	Busy  bool   `json:"busy"`
	Stage string `json:"stage"`
	State string `json:"state,omitempty"`
	Error string `json:"error,omitempty"`
	Done  int64  `json:"done"`
	Total int64  `json:"total"`
}

type InstalledRuntime struct {
	Version string `json:"version"`
	SHA256  string `json:"sha256"`
}

type Manager struct {
	profile         profileAdmission
	ClientVersion   string
	OpenBrowser     func(string) error
	Root            string
	DataRoot        string
	Endpoint        string
	HTTPClient      *http.Client
	modelHTTPClient *http.Client
	modelMu         sync.Mutex
	modelDownloads  map[modelVariant]bool
	localClient     *http.Client
	mu              sync.Mutex
	directRuntime   HealthInfo
	hostAuthority   runtimeHostAuthority
	startupControl  func(context.Context, InstalledRuntime) (*runtimeStartup, error)
	cliInstallMu    sync.RWMutex
	prefs           Preferences
	completedTasks  []TaskState
	taskChanged     chan struct{}
	task            TaskState
	taskCancel      context.CancelFunc
	taskPause       bool
	jobs            map[string]*Operation
	// proxy resolves environment and system proxy settings for outbound
	// requests. localClient deliberately bypasses it (loopback only).
	proxy *proxyResolver
}

func New() (*Manager, error) {
	port := os.Getenv("RSRS_RPC_PORT")
	if port == "" {
		port = "15169"
	}
	n, err := strconv.Atoi(port)
	if err != nil || n < 1 || n > 65535 {
		return nil, errors.New("RSRS_RPC_PORT must be a valid TCP port")
	}
	dataRoot, err := clientDataRoot()
	if err != nil {
		return nil, err
	}
	profile, release, err := admitProfile(dataRoot, os.Getenv("RSRS_DATA_DIR"))
	if err != nil {
		return nil, err
	}
	defer release()
	// Transfer identity handles to the manager only after initialization.
	initialized := false
	defer func() {
		if !initialized {
			profile.close()
		}
	}()
	if profile.reopen != nil {
		for _, name := range []string{"preferences.json", "requests.json"} {
			if err := checkModelUninstallPath(filepath.Join(profile.root, "client2", name)); err != nil {
				return nil, err
			}
		}
	}
	m, err := newCLIInstaller(profile.root)
	if err != nil {
		return nil, err
	}
	m.profile = profile
	m.Endpoint = "http://127.0.0.1:" + port
	m.startupControl = m.acquireStartup
	m.localClient = &http.Client{Timeout: 30 * time.Second, Transport: &http.Transport{Proxy: nil}, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	if err := m.loadOperations(); err != nil {
		return nil, err
	}
	initialized = true
	return m, nil
}

// NewCLIInstaller reads the shared installer settings for an explicit CLI update.
func NewCLIInstaller() (*Manager, error) {
	root, err := clientDataRoot()
	if err != nil {
		return nil, err
	}
	return newCLIInstaller(root)
}

func clientDataRoot() (string, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return "", err
	}
	dataRoot := os.Getenv("RSRS_DATA_DIR")
	if dataRoot == "" {
		dataRoot = filepath.Join(home, ".rsrs")
	}
	return filepath.Abs(dataRoot)
}

func newCLIInstaller(dataRoot string) (*Manager, error) {
	m := &Manager{Root: filepath.Join(dataRoot, "client2"), DataRoot: dataRoot,
		prefs: defaultPreferences(), task: TaskState{Stage: i18n.Message56df21b4022f720d},
		proxy: newProxyResolver()}
	proxyFor := m.proxy.proxyFunc(func() string { return m.Preferences().ProxyMode })
	// Both outbound clients share the resolver: updates, release metadata and
	// model weights follow the same environment/system proxy. The defaults
	// (dial and TLS timeouts, HTTP/2) are kept from http.DefaultTransport;
	// downloads have no overall deadline (the request context governs) and
	// HTTPS-only redirects.
	outbound := http.DefaultTransport.(*http.Transport).Clone()
	outbound.Proxy = proxyFor
	m.HTTPClient = &http.Client{Timeout: 10 * time.Minute, Transport: outbound, CheckRedirect: func(req *http.Request, via []*http.Request) error {
		if len(via) >= 5 {
			return errors.New("too many redirects")
		}
		if req.URL.Scheme != "https" {
			return errors.New("download redirects must use HTTPS")
		}
		return nil
	}}
	m.modelHTTPClient = &http.Client{Transport: outbound.Clone()}
	if err := os.MkdirAll(m.Root, 0700); err != nil {
		return nil, err
	}
	prefs, err := loadPreferencesFile(filepath.Join(m.Root, "preferences.json"))
	if err != nil {
		return nil, err
	}
	m.prefs = prefs
	return m, nil
}

// loadPreferencesFile reads one preferences file: a missing file means a
// brand-new installation that keeps the first-run wizard state, while an
// existing file without wizard fields belongs to an upgraded user who is
// migrated to completed.
func loadPreferencesFile(path string) (Preferences, error) {
	b, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		prefs := defaultPreferences()
		if err := prefs.validate(); err != nil {
			return prefs, err
		}
		return prefs, nil
	}
	if err != nil {
		return Preferences{}, fmt.Errorf("read preferences: %w", err)
	}
	return loadPreferences(b)
}

func loadPreferences(b []byte) (Preferences, error) {
	var stored map[string]json.RawMessage
	if err := json.Unmarshal(b, &stored); err != nil {
		return Preferences{}, fmt.Errorf("read preferences: %w", err)
	}
	prefs := defaultPreferences()
	if err := json.Unmarshal(b, &prefs); err != nil {
		return Preferences{}, fmt.Errorf("read preferences: %w", err)
	}
	prefs = migratePreferences(prefs, stored)
	if err := prefs.validate(); err != nil {
		return Preferences{}, err
	}
	return prefs, nil
}

func writeJSON(path string, value any) error {
	b, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return err
	}
	f, err := os.CreateTemp(filepath.Dir(path), ".write-*")
	if err != nil {
		return err
	}
	tmp := f.Name()
	defer os.Remove(tmp)
	if _, err = f.Write(append(b, '\n')); err != nil {
		f.Close()
		return err
	}
	if err = f.Sync(); err != nil {
		f.Close()
		return err
	}
	if err = f.Close(); err != nil {
		return err
	}
	return os.Rename(tmp, path)
}

func (m *Manager) Preferences() Preferences { m.mu.Lock(); defer m.mu.Unlock(); return m.prefs }

func (m *Manager) SavePreferences(p Preferences) error {
	mirror, err := NormalizeModelMirror(p.ModelMirror)
	if err != nil {
		return err
	}
	p.ModelMirror = mirror
	if err := p.validate(); err != nil {
		return err
	}
	m.mu.Lock()
	defer m.mu.Unlock()
	if err := writeJSON(filepath.Join(m.Root, "preferences.json"), p); err != nil {
		return err
	}
	m.prefs = p
	return nil
}

func (m *Manager) progress(stage string, done, total int64) {
	m.mu.Lock()
	defer m.mu.Unlock()
	m.task.Stage, m.task.Done, m.task.Total = stage, done, total
	m.notifyTaskChanged()
}

func (m *Manager) RunTask(stage string, action func() error) error {
	m.mu.Lock()
	if m.task.Busy {
		m.mu.Unlock()
		return errors.New(i18n.Message47e470f70412a5d3)
	}
	previous := m.task
	m.task = TaskState{Busy: true, Stage: stage}
	m.mu.Unlock()
	err := action()
	m.mu.Lock()
	defer m.mu.Unlock()
	if errors.Is(err, errRuntimeUpdateDeferred) {
		m.task = previous
		return nil
	}
	m.task.Busy = false
	if err != nil {
		m.task.Stage = i18n.Messagebf50abaf51e2a82f
		m.task.Error = err.Error()
	} else {
		m.task.Stage = i18n.Message5a73ae60f30b9728
	}
	return err
}

// StartDownload publishes the busy snapshot before returning its receipt.
func (m *Manager) StartDownload(stage string, action func(context.Context) error) string {
	m.mu.Lock()
	if m.task.Busy {
		m.mu.Unlock()
		return ""
	}
	ctx, cancel := context.WithCancel(context.Background())
	m.taskCancel = cancel
	m.taskPause = false
	id := rand.Text()
	m.task = TaskState{ID: id, Busy: true, Stage: stage}
	m.mu.Unlock()
	go func() {
		err := action(ctx)
		m.mu.Lock()
		defer m.mu.Unlock()
		defer m.notifyTaskChanged()
		m.taskCancel = nil
		m.task.Busy = false
		paused := m.taskPause && errors.Is(err, context.Canceled)
		m.taskPause = false
		switch {
		case err == nil:
			m.task.Stage = i18n.Message5a73ae60f30b9728
			m.task.State = "downloaded"
		case paused:
			m.task.Stage = i18n.Message563ea3a387e4cd9b
		case errors.Is(err, context.Canceled):
			m.task.Stage = i18n.Message91962c6770277412
		default:
			m.task.Stage = i18n.Messagebf50abaf51e2a82f
			m.task.Error = err.Error()
		}
		m.completedTasks = append(m.completedTasks, m.task)
		if len(m.completedTasks) > 8 {
			m.completedTasks = m.completedTasks[1:]
		}
	}()
	return id
}

func (m *Manager) TaskSnapshot() TaskState { m.mu.Lock(); defer m.mu.Unlock(); return m.task }

// WatchTask captures the snapshot and its next change under the same lock.
func (m *Manager) WatchTask(id string) (TaskState, <-chan struct{}, bool) {
	m.mu.Lock()
	defer m.mu.Unlock()
	if id != "" && id == m.task.ID && m.task.Busy {
		if m.taskChanged == nil {
			m.taskChanged = make(chan struct{})
		}
		return m.task, m.taskChanged, true
	}
	for _, task := range m.completedTasks {
		if task.ID == id {
			return task, nil, true
		}
	}
	return TaskState{}, nil, false
}

func (m *Manager) CancelDownload(id string, pause bool) bool {
	m.mu.Lock()
	defer m.mu.Unlock()
	if id == "" || id != m.task.ID || !m.task.Busy || m.taskCancel == nil {
		return false
	}
	m.taskPause = pause
	m.taskCancel()
	return true
}

// Called with m.mu held. Slow readers coalesce updates into the latest snapshot.
func (m *Manager) notifyTaskChanged() {
	if m.taskChanged != nil {
		close(m.taskChanged)
		m.taskChanged = nil
	}
}
