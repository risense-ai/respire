package desktop

import "respire-client2/internal/i18n"

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"math"
	"net/http"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
	"time"

	"respire-client2/internal/host"
)

func (d *desktop) businessRoute(w http.ResponseWriter, r *http.Request) bool {
	if d.runtimeOperationRoute(w, r) {
		return true
	}
	var result any
	var err error
	switch {
	case r.Method == "GET" && r.URL.Path == "/api/task-events":
		d.taskStreams.serve(w, r, d.m, d.closing)
		return true
	case r.Method == "GET" && r.URL.Path == "/api/task":
		task, _, found := d.m.WatchTask(r.URL.Query().Get("id"))
		if !found {
			w.WriteHeader(http.StatusGone)
		}
		result = task
	case r.Method == "GET" && r.URL.Path == "/api/fonts":
		result, err = SystemFonts()
	case r.Method == "GET" && r.URL.Path == "/api/runtime-status":
		ctx, cancel := context.WithTimeout(r.Context(), 6*time.Second)
		defer cancel()
		result, err = d.m.RuntimeStatus(ctx)
	case r.Method == "GET" && r.URL.Path == "/api/operations":
		result = d.m.Operations()
	case r.Method == "GET" && r.URL.Path == "/api/plugins":
		ctx, cancel := context.WithTimeout(r.Context(), 2*time.Second)
		defer cancel()
		result = d.PluginStatuses(ctx)
	case r.Method == "GET" && r.URL.Path == "/api/inject-targets":
		ctx, cancel := context.WithTimeout(r.Context(), 10*time.Second)
		defer cancel()
		result, err = d.InjectTargetsView(ctx)
	case r.Method == "GET" && r.URL.Path == "/api/runtime-release":
		ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
		defer cancel()
		_, metadata, releaseErr := d.m.LatestRuntime(ctx)
		err = releaseErr
		result = map[string]any{"version": metadata.RuntimeVersion, "channel": d.m.Preferences().RuntimeChannel}
	case r.Method == "GET" && r.URL.Path == "/api/model-variants":
		ctx, cancel := context.WithTimeout(r.Context(), 4*time.Second)
		defer cancel()
		result, err = d.m.ModelVariants(ctx, r.URL.Query().Get("model"))
	case r.Method == "GET" && r.URL.Path == "/api/model-profile-state":
		ctx, cancel := context.WithTimeout(r.Context(), 6*time.Second)
		defer cancel()
		catalog, catalogErr := d.m.ModelCatalogRPC(ctx)
		progress, progressErr := d.m.ModelProgress(ctx)
		result = map[string]any{"catalog": catalog, "progress": progress}
		err = catalogErr
		if err == nil {
			err = progressErr
		}
	case r.URL.Path == "/api/model-configure":
		if r.Method != "POST" || r.Header.Get("X-Respire-Client") != "1" {
			w.WriteHeader(http.StatusForbidden)
			err = errors.New(i18n.Message0a8050e64dca06f2)
			break
		}
		var body struct {
			Model   string `json:"model"`
			Variant string `json:"variant"`
		}
		if err = json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096)).Decode(&body); err != nil {
			break
		}
		ctx, cancel := context.WithTimeout(r.Context(), 20*time.Second)
		defer cancel()
		err = d.m.ModelConfigure(ctx, body.Model, body.Variant)
		result = i18n.Messaged502ce7316e94834
	case r.URL.Path == "/api/model-switch":
		if r.Method != "POST" || r.Header.Get("X-Respire-Client") != "1" {
			w.WriteHeader(http.StatusForbidden)
			err = errors.New(i18n.Message0a8050e64dca06f2)
			break
		}
		var body struct {
			Model   string `json:"model"`
			Engine  string `json:"engine"`
			Variant string `json:"variant"`
		}
		if err = json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096)).Decode(&body); err != nil {
			break
		}
		if body.Engine == "" {
			body.Engine = "cpu"
		}
		ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
		defer cancel()
		result, err = d.m.StartModelSwitch(ctx, body.Model, body.Engine, body.Variant)
	case r.URL.Path == "/api/model-download":
		if r.Method != "POST" || r.Header.Get("X-Respire-Client") != "1" {
			w.WriteHeader(http.StatusForbidden)
			err = errors.New(i18n.Message0a8050e64dca06f2)
			break
		}
		var body struct {
			Model   string `json:"model"`
			Variant string `json:"variant"`
			Mirror  string `json:"mirror"`
		}
		if err = json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096)).Decode(&body); err != nil {
			break
		}
		if body.Model == "" {
			body.Model = "m3"
		}
		ctx, cancel := context.WithTimeout(r.Context(), 30*time.Second)
		defer cancel()
		receipt, managed, startErr := d.m.StartRuntimeModelDownload(ctx, body.Model, body.Variant, body.Mirror)
		if startErr != nil {
			err = startErr
			break
		}
		if managed {
			result = map[string]any{"id": receipt.OperationId, "runtime": true, "stream": receipt.Stream}
			break
		}
		model := body.Model
		variant := body.Variant
		mirror := body.Mirror
		// Start in the background so the POST returns before the download
		// finishes; otherwise progress polling could only begin after the
		// task was already over and the modal never showed a live bar.
		id := d.m.StartDownload(i18n.Message446478c6a451bc64+variant, func(ctx context.Context) error {
			ctx, cancel := context.WithTimeout(ctx, 60*time.Minute)
			defer cancel()
			return d.m.DownloadModel(ctx, model, variant, mirror)
		})
		if id == "" {
			w.WriteHeader(http.StatusConflict)
			err = errors.New(i18n.Message7f7d5e3ef44921fb)
			break
		}
		result = map[string]string{"id": id}
	case r.URL.Path == "/api/model-download-cancel":
		if r.Method != "POST" || r.Header.Get("X-Respire-Client") != "1" {
			w.WriteHeader(http.StatusForbidden)
			err = errors.New(i18n.Message0a8050e64dca06f2)
			break
		}
		var body struct {
			ID    string `json:"id"`
			Pause bool   `json:"pause"`
		}
		if err = json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096)).Decode(&body); err != nil {
			break
		}
		if !d.m.CancelDownload(body.ID, body.Pause) {
			w.WriteHeader(http.StatusConflict)
			err = errors.New("Download is no longer active")
			break
		}
		result = i18n.Messageb4504619611a78dc
	case r.Method == "GET" && r.URL.Path == "/api/operation":
		result, err = d.m.Operation(r.URL.Query().Get("id"))
	case r.URL.Path == "/api/execute" || r.URL.Path == "/api/operation-recover" || r.URL.Path == "/api/operation-continue" || r.URL.Path == "/api/operation-ack" || r.URL.Path == "/api/recovery-material-ack" || r.URL.Path == "/api/file-dialog" || r.URL.Path == "/api/open-dashboard" || r.URL.Path == "/api/open-main" || r.URL.Path == "/api/panel-size" || r.URL.Path == "/api/cli-language" || r.URL.Path == "/api/plugin-install" || r.URL.Path == "/api/inject-set" || r.URL.Path == "/api/inject-mode" || r.URL.Path == "/api/preferences-export" || r.URL.Path == "/api/preferences-import" || r.URL.Path == "/api/diagnostics-export" || r.URL.Path == "/api/open-model-dir":
		if r.Method != "POST" || r.Header.Get("X-Respire-Client") != "1" || r.Header.Get("Content-Type") != "application/json" {
			w.WriteHeader(http.StatusForbidden)
			err = errors.New(i18n.Message0a8050e64dca06f2)
			break
		}
		var input struct {
			Name        string          `json:"name"`
			ID          string          `json:"id"`
			TransportID string          `json:"transportId"`
			Args        json.RawMessage `json:"args"`
			Confirmed   bool            `json:"confirmed"`
			Secret      string          `json:"secret"`
			Kind        string          `json:"kind"`
			Language    string          `json:"language"`
			Target      string          `json:"target"`
			Mirror      bool            `json:"mirror"`
			Path        string          `json:"path"`
			Height      float64         `json:"height"`
			Enable      bool            `json:"enable"`
			Mode        string          `json:"mode"`
			Scope       string          `json:"scope"`
		}
		decoder := json.NewDecoder(http.MaxBytesReader(w, r.Body, 1<<20))
		decoder.DisallowUnknownFields()
		if err = decoder.Decode(&input); err != nil {
			break
		}
		var trailing any
		if decoder.Decode(&trailing) != io.EOF {
			err = errors.New(i18n.Messaged8d27c196a37710e)
			break
		}
		switch r.URL.Path {
		case "/api/execute":
			result, err = d.m.Submit(input.Name, input.ID, input.Args, input.Confirmed, input.Secret)
		case "/api/operation-recover":
			result, err = d.m.Recover(input.ID)
		case "/api/operation-continue":
			if !input.Confirmed {
				err = errors.New(i18n.Message8a9d7f7e44d45996)
			} else {
				result, err = d.m.Continue(input.ID, input.TransportID, input.Args)
			}
		case "/api/operation-ack":
			err = d.m.Acknowledge(input.ID)
			result = i18n.Messagef25d87b7046229b8
		case "/api/recovery-material-ack":
			if !input.Confirmed {
				err = errors.New(i18n.Messageaa26c60c18de075a)
			} else {
				err = d.m.AcknowledgeRecoveryMaterial(input.ID)
				result = i18n.Messagebc6212c007be96f5
			}
		case "/api/file-dialog":
			result, err = d.chooseFile(input.Kind)
		case "/api/open-dashboard":
			err = d.app.Browser.OpenURL("https://dash.rsrs.rs")
			result = i18n.Messagefba6b9761cb1a021
		case "/api/open-main":
			if d.panel != nil {
				d.panel.Hide()
			}
			d.window.Show()
			d.window.Focus()
			result = i18n.Messagebbc5abe8f949585a
		case "/api/panel-size":
			height := int(math.Round(input.Height))
			if height < 320 {
				height = 320
			}
			if height > 1100 {
				height = 1100
			}
			if d.panel != nil {
				d.panel.SetSize(360, height)
			}
			result = height
		case "/api/open-releases":
			url := map[string]string{
				"client":        "https://github.com/risense-ai/respire-client2/releases/latest",
				"client-mirror": "https://gh-proxy.com/https://github.com/risense-ai/respire-client2/releases/latest",
				"cli":           "https://github.com/risense-ai/respire-cli/releases/latest",
				"cli-mirror":    "https://gh-proxy.com/https://github.com/risense-ai/respire-cli/releases/latest",
			}[input.Target]
			if url == "" && strings.HasPrefix(input.Target, "plugin-") {
				repo := strings.TrimPrefix(input.Target, "plugin-")
				if strings.HasPrefix(repo, "mirror-") {
					repo = strings.TrimPrefix(repo, "mirror-")
					if _, ok := pluginByID(repo); ok {
						url = "https://gh-proxy.com/https://github.com/risense-ai/" + repo
					}
				} else {
					if _, ok := pluginByID(repo); ok {
						url = "https://github.com/risense-ai/" + repo
					}
				}
			}
			if url == "" {
				err = errors.New(i18n.Message6cb2dab1c83226a7)
				break
			}
			err = d.app.Browser.OpenURL(url)
			result = i18n.Messagea5939c53799f0cf9
		case "/api/cli-language":
			err = d.m.SetCLILanguage(input.Language)
			result = i18n.Messagea04d157f09029173
		case "/api/plugin-install":
			ctx, cancel := context.WithTimeout(r.Context(), 10*time.Minute)
			defer cancel()
			result, err = d.InstallPlugin(ctx, input.Target, input.Mirror)
		case "/api/open-model-dir":
			// Reveal a model file in the OS file manager; only paths inside the
			// model directory are accepted.
			dir := d.m.ModelDir()
			abs, absErr := filepath.Abs(input.Path)
			if absErr != nil {
				err = absErr
				break
			}
			rel, relErr := filepath.Rel(dir, abs)
			if relErr != nil || rel == ".." || strings.HasPrefix(rel, ".."+string(filepath.Separator)) {
				err = errors.New(i18n.Messagea32ad328c9836b84)
				break
			}
			var cmd *exec.Cmd
			switch runtime.GOOS {
			case "windows":
				cmd = exec.Command("explorer", "/select,"+abs)
			case "darwin":
				cmd = exec.Command("open", "-R", abs)
			default:
				cmd = exec.Command("xdg-open", filepath.Dir(abs))
			}
			if err = cmd.Start(); err == nil {
				result = i18n.Message2ee5cba974e9f728
			}
		case "/api/inject-set":
			ctx, cancel := context.WithTimeout(r.Context(), 10*time.Minute)
			defer cancel()
			result, err = d.InjectSet(ctx, input.ID, input.Enable, input.Mirror, input.Scope)
		case "/api/inject-mode":
			ctx, cancel := context.WithTimeout(r.Context(), 2*time.Minute)
			defer cancel()
			result, err = d.InjectSetMode(ctx, input.Mode)
		case "/api/preferences-export":
			err = writeSettingsExport(input.Path, d.m.Preferences())
			result = i18n.Message811a08bc23a7eebd
		case "/api/preferences-import":
			var p host.Preferences
			data, readErr := os.ReadFile(input.Path)
			if readErr != nil {
				err = readErr
				break
			}
			if len(data) > 64<<10 {
				err = errors.New(i18n.Message990f7d95a8e1fc95)
				break
			}
			var stored map[string]json.RawMessage
			if err = json.Unmarshal(data, &stored); err == nil {
				if err = json.Unmarshal(data, &p); err == nil {
					err = d.savePreferences(host.MigratePreferences(p, stored))
				}
			}
			result = i18n.Messagec4efeafc7a8e98d5
		case "/api/diagnostics-export":
			ctx, cancel := context.WithTimeout(r.Context(), 5*time.Second)
			health, _, healthErr := d.m.Health(ctx)
			cancel()
			auto, _ := d.app.Autostart.IsEnabled()
			info, _ := d.m.Installed()
			d.mu.Lock()
			update := map[string]any{
				"state": d.app.Updater.State(), "latest": d.clientVersion, "source": d.clientSource,
				"notes": d.clientNotes, "error": d.clientError,
			}
			d.mu.Unlock()
			report := map[string]any{
				"generatedAt": time.Now().UTC(), "clientVersion": version, "platform": host.Platform(),
				"endpoint": d.m.Endpoint, "root": d.m.Root, "preferences": d.m.Preferences(),
				"proxy":     d.m.ProxyInfo(),
				"autostart": auto, "task": d.m.TaskSnapshot(), "connected": healthErr == nil,
				"installed": info, "health": health, "healthError": errorText(healthErr),
				"clientUpdate": update, "operations": d.m.Operations(),
			}
			err = writeJSONFile(input.Path, report)
			result = i18n.Message6da4b8699c0c6609
		}
	default:
		return false
	}
	if err != nil {
		json.NewEncoder(w).Encode(map[string]any{"ok": false, "error": err.Error()})
	} else {
		json.NewEncoder(w).Encode(map[string]any{"ok": true, "result": result})
	}
	return true
}

// preferencePatchKeys is the canonical allowlist of patchable preference
// keys. patchPreferences applies them and api.go's actionInput shape check
// carries a matching JSON field for each, so the two lists cannot drift.
var preferencePatchKeys = []string{
	"autoRuntime", "autoUpdate", "runtimeChannel", "clientChannel",
	"layout", "theme", "language", "proxyMode", "clientUpdateInterval", "runtimeUpdateInterval",
	"clientAutoUpdate", "clientAutoRestart", "runtimeAutoUpdate", "runtimeAutoRestart",
	"sidebarWidth", "memoryIndexWidth",
	"onboardingVersion", "onboardingCompleted", "onboardingStep", "onboardingSyncChoice",
}

// preferencePatchTargets maps each allowlisted key onto its Preferences
// field for patchPreferences.
func preferencePatchTargets(p *host.Preferences) map[string]any {
	return map[string]any{
		"autoRuntime":           &p.AutoRuntime,
		"autoUpdate":            &p.AutoUpdate,
		"runtimeChannel":        &p.RuntimeChannel,
		"clientChannel":         &p.ClientChannel,
		"layout":                &p.Layout,
		"theme":                 &p.Theme,
		"language":              &p.Language,
		"proxyMode":             &p.ProxyMode,
		"clientUpdateInterval":  &p.ClientUpdateInterval,
		"runtimeUpdateInterval": &p.RuntimeUpdateInterval,
		"clientAutoUpdate":      &p.ClientAutoUpdate,
		"clientAutoRestart":     &p.ClientAutoRestart,
		"runtimeAutoUpdate":     &p.RuntimeAutoUpdate,
		"runtimeAutoRestart":    &p.RuntimeAutoRestart,
		"sidebarWidth":          &p.SidebarWidth,
		"memoryIndexWidth":      &p.MemoryIndexWidth,
		"onboardingVersion":     &p.OnboardingVersion,
		"onboardingCompleted":   &p.OnboardingCompleted,
		"onboardingStep":        &p.OnboardingStep,
		"onboardingSyncChoice":  &p.OnboardingSyncChoice,
	}
}

// patchPreferences overlays only the fields present in the request body onto
// the stored preferences, so partial form posts never reset fields they do
// not render.
func patchPreferences(base host.Preferences, body []byte) (host.Preferences, error) {
	var keys map[string]json.RawMessage
	if err := json.Unmarshal(body, &keys); err != nil {
		return base, err
	}
	p := base
	targets := preferencePatchTargets(&p)
	for _, key := range preferencePatchKeys {
		raw, ok := keys[key]
		if !ok {
			continue
		}
		if err := json.Unmarshal(raw, targets[key]); err != nil {
			return base, err
		}
	}
	return p, nil
}

func writeSettingsExport(path string, value host.Preferences) error {
	data, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, append(data, '\n'), 0600)
}

func writeJSONFile(path string, value any) error {
	data, err := json.MarshalIndent(value, "", "  ")
	if err != nil {
		return err
	}
	return os.WriteFile(path, append(data, '\n'), 0600)
}

func errorText(err error) string {
	if err == nil {
		return ""
	}
	return err.Error()
}
