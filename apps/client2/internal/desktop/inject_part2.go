package desktop

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"respire-client2/internal/i18n"
	"runtime"
	"slices"
	"strings"
)

// patchPromptAppend adds/removes the inject path in each agent's prompt_append
// in oh-my-openagent.json (no-op if the field is missing).
func patchPromptAppend(e injectHome, path string, add bool) (bool, error) {
	textBytes, err := os.ReadFile(path)
	if err != nil {
		return false, err
	}
	cfg, err := parseJSONObject(textBytes)
	if err != nil {
		return false, fmt.Errorf(i18n.Messageb156fc7f649faf2f, path, err)
	}
	abs := e.entityPath()
	line := i18n.Message09b0cb0374e61ca1 + abs
	var walk func(any) any
	walk = func(value any) any {
		switch item := value.(type) {
		case map[string]any:
			if s, ok := item["prompt_append"].(string); ok {
				if add && !strings.Contains(s, abs) {
					if s != "" && !strings.HasSuffix(s, "\n") {
						s += "\n"
					}
					item["prompt_append"] = s + line
				} else if !add {
					kept := make([]string, 0, 4)
					for _, l := range strings.Split(s, "\n") {
						if l != line {
							kept = append(kept, l)
						}
					}
					item["prompt_append"] = strings.Join(kept, "\n")
				}
			}
			for key, child := range item {
				item[key] = walk(child)
			}
		case []any:
			for i, child := range item {
				item[i] = walk(child)
			}
		}
		return value
	}
	updated := walk(cfg).(map[string]any)
	data, err := jsonPretty(updated)
	if err != nil {
		return false, err
	}
	if string(data) == string(textBytes) {
		return false, nil
	}
	return true, os.WriteFile(path, data, 0o644)
}

func writeBlock(e injectHome, path string) (bool, error) {
	return applyBlockWith(path, false, e.instructions())
}

func removeBlock(e injectHome, path string) (bool, error) {
	return applyBlockWith(path, true, e.instructions())
}

// workbuddy reference mode: entity file (shared with opencode) + MEMORY.md
// reference block.
func injectWorkbuddy(e injectHome) (bool, error) {
	changed, err := writeCover(e, e.entityPath())
	if err != nil {
		return false, err
	}
	item, err := applyBlockWith(joinHome(e.home, ".workbuddy", "MEMORY.md"), false, workbuddyRefMD(e.entityPath()))
	if err != nil {
		return false, err
	}
	return changed || item, nil
}

type blockPreview struct {
	path    string
	before  string
	after   string
	changed bool
}

func blockPreviewWith(path string, remove bool, content string) (blockPreview, error) {
	beforeBytes, err := os.ReadFile(path)
	if err != nil && !os.IsNotExist(err) {
		return blockPreview{}, err
	}
	before := string(beforeBytes)
	type marker struct{ start, end int }
	var bounds []marker
	for _, pair := range [][2]string{{blockBegin, blockEnd}, {legacyBlockBegin, legacyBlockEnd}} {
		starts := indicesOf(before, pair[0])
		ends := indicesOf(before, pair[1])
		switch {
		case len(starts) == 0 && len(ends) == 0:
		case len(starts) == 1 && len(ends) == 1 && starts[0] < ends[0]:
			bounds = append(bounds, marker{starts[0], ends[0] + len(pair[1])})
		default:
			return blockPreview{}, fmt.Errorf(i18n.Message61b5adcf1c2455bc, path)
		}
	}
	slices.SortFunc(bounds, func(a, b marker) int { return a.start - b.start })
	for i := 1; i < len(bounds); i++ {
		if bounds[i-1].end > bounds[i].start {
			return blockPreview{}, fmt.Errorf(i18n.Message648c83b7fe954f7d, path)
		}
	}
	newline := "\n"
	if i := strings.IndexByte(before, '\n'); i > 0 && before[i-1] == '\r' {
		newline = "\r\n"
	}
	block := blockBegin + newline + trimSpaceRight(content) + newline + blockEnd
	var after string
	if len(bounds) == 0 {
		if remove {
			after = before
		} else {
			var sep string
			switch {
			case before == "":
			case strings.HasSuffix(before, "\n\n"):
			case strings.HasSuffix(before, "\n"):
				sep = newline
			default:
				sep = newline + newline
			}
			after = before + sep + block + newline
		}
	} else {
		var b strings.Builder
		cursor := 0
		for index, m := range bounds {
			b.WriteString(before[cursor:m.start])
			if index == 0 && !remove {
				b.WriteString(block)
			}
			cursor = m.end
		}
		b.WriteString(before[cursor:])
		after = b.String()
	}
	return blockPreview{path: path, before: before, after: after, changed: before != after}, nil
}

func applyBlockWith(path string, remove bool, content string) (bool, error) {
	preview, err := blockPreviewWith(path, remove, content)
	if err != nil {
		return false, err
	}
	if !preview.changed {
		return false, nil
	}
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return false, err
	}
	return true, os.WriteFile(path, []byte(preview.after), 0o644)
}

// hostPluginRepo maps a prompt-injection host to its Respire plugin repo (both
// are released together when the agent is enabled).
var hostPluginRepo = map[string]string{
	"dsh":       "respire-dsh",
	"codex":     "respire-codex",
	"workbuddy": "respire-workbuddy",
	"pi":        "respire-pi",
	"zcode":     "respire-zcode",
}

// pluginOnlyHosts have a plugin but no prompt target of their own.
var pluginOnlyHosts = []struct{ id, name, repo string }{
	{"magpie", "magpie", "respire-magpie"},
	{"qoder", "qoder", "respire-qoder"},
	{"kimi", "kimi code", "respire-kimi"},
	{"dimcode", "dim code", "respire-dimcode"},
	{"hermes", "hermes", "respire-hermes"},
}

// InjectTargetsView lists every agent row: prompt targets (with their plugin,
// if any) plus the plugin-only hosts.
func (d *desktop) InjectTargetsView(ctx context.Context) (map[string]any, error) {
	e, err := d.injectHome()
	if err != nil {
		return nil, err
	}
	targets := injectTargets(e)
	plugins := map[string]*pluginStatus{}
	for _, item := range d.PluginStatuses(ctx) {
		plugin := item
		plugins[plugin.ID] = &plugin
	}
	for i := range targets {
		if repo, ok := hostPluginRepo[targets[i].ID]; ok {
			targets[i].Plugin = plugins[repo]
		}
	}
	for _, host := range pluginOnlyHosts {
		plugin := plugins[host.repo]
		row := injectTarget{
			ID:     host.id,
			Name:   host.name,
			Mode:   "plugin",
			Path:   d.pluginPath(host.repo),
			Plugin: plugin,
		}
		row.RealPath = row.Path
		row.LikelyInstalled = plugin != nil && (plugin.Source || plugin.Command)
		if plugin != nil && plugin.Source {
			row.State = "fresh"
		} else {
			row.State = "absent"
		}
		targets = append(targets, row)
	}
	return map[string]any{"mode": e.mode, "targets": targets}, nil
}

// InjectSet enables or disables one agent. Scope selects what is released:
// "all" (the row switch: prompt + plugin), "prompt" (write/remove the prompt
// only) or "plugin" (install/update the plugin only). Disable always means
// removing the injected prompt — plugins stay and must be removed in the host
// UI, so scope=plugin only supports enable.
func (d *desktop) InjectSet(ctx context.Context, id string, enable, mirror bool, scope string) (map[string]any, error) {
	if scope == "" {
		scope = "all"
	}
	if scope != "all" && scope != "prompt" && scope != "plugin" {
		return nil, fmt.Errorf(i18n.Message10e949ec96972180, scope)
	}
	for _, host := range pluginOnlyHosts {
		if host.id == id {
			if scope == "prompt" {
				return nil, errors.New(i18n.Message55f7a4dbbf34f83a)
			}
			if !enable {
				return nil, errors.New(i18n.Message66b946a4b539598d)
			}
			message, _, err := d.ensurePlugin(ctx, host.repo, mirror)
			if err != nil {
				return nil, err
			}
			return map[string]any{"id": id, "enabled": true, "message": message}, nil
		}
	}
	e, err := d.injectHome()
	if err != nil {
		return nil, err
	}
	if scope == "plugin" {
		repo, ok := hostPluginRepo[id]
		if !ok {
			return nil, errors.New(i18n.Messaged6aab5b002d9682a)
		}
		message, _, pluginErr := d.ensurePlugin(ctx, repo, mirror)
		if pluginErr != nil {
			return nil, pluginErr
		}
		return map[string]any{"id": id, "enabled": true, "plugin": map[string]any{"ok": true, "message": message}}, nil
	}
	var changed bool
	if enable {
		changed, err = injectOne(e, id)
	} else {
		changed, err = removeOne(e, id)
	}
	if err != nil {
		return nil, err
	}
	result := map[string]any{"id": id, "enabled": enable, "changed": changed}
	if repo, ok := hostPluginRepo[id]; ok && enable && scope == "all" {
		message, _, pluginErr := d.ensurePlugin(ctx, repo, mirror)
		pluginResult := map[string]any{"ok": pluginErr == nil, "message": message}
		if pluginErr != nil {
			pluginResult["message"] = pluginErr.Error()
		}
		result["plugin"] = pluginResult
	}
	return result, nil
}

// InjectSetMode saves the workspace mode and rewrites the prompts of every
// currently injected target so the injected edition follows the mode.
func (d *desktop) InjectSetMode(ctx context.Context, mode string) (map[string]any, error) {
	if err := d.m.SetWorkspaceMode(ctx, mode); err != nil {
		return nil, err
	}
	e, err := d.injectHome()
	if err != nil {
		return nil, err
	}
	results := []map[string]any{}
	for _, target := range injectTargets(e) {
		switch target.State {
		case "fresh", "stale":
			changed, err := injectOne(e, target.ID)
			item := map[string]any{"id": target.ID, "ok": err == nil, "changed": changed}
			if err != nil {
				item["error"] = err.Error()
			}
			results = append(results, item)
		}
	}
	return map[string]any{"mode": e.mode, "updated": len(results), "results": results}, nil
}

func (d *desktop) injectHome() (injectHome, error) {
	home, err := os.UserHomeDir()
	if err != nil {
		return injectHome{}, err
	}
	mode, err := d.m.WorkspaceMode()
	if err != nil {
		return injectHome{}, err
	}
	return injectHome{home: home, localData: localDataDir(), mode: mode}, nil
}

func localDataDir() string {
	if runtime.GOOS == "windows" {
		return os.Getenv("LOCALAPPDATA")
	}
	return ""
}

func joinPath(elem ...string) string { return filepath.Join(elem...) }

func fileExists(path string) bool {
	_, err := os.Lstat(path)
	return err == nil
}

func indicesOf(text, sub string) []int {
	var out []int
	for i := 0; ; {
		j := strings.Index(text[i:], sub)
		if j < 0 {
			return out
		}
		i += j
		out = append(out, i)
		i += len(sub)
	}
}

// parseJSONObject reads a JSON object; anything else (or a parse failure) is
// an error.
func parseJSONObject(data []byte) (map[string]any, error) {
	var value map[string]any
	if err := json.Unmarshal(data, &value); err != nil {
		return nil, err
	}
	if value == nil {
		return nil, errors.New(i18n.Messageb8db62370092320f)
	}
	return value, nil
}

// jsonPretty renders compact-stable pretty JSON (2-space indent, no HTML
// escaping) to match what the CLI writes for the same files.
func jsonPretty(value any) ([]byte, error) {
	var buf bytes.Buffer
	encoder := json.NewEncoder(&buf)
	encoder.SetEscapeHTML(false)
	encoder.SetIndent("", "  ")
	if err := encoder.Encode(value); err != nil {
		return nil, err
	}
	return bytes.TrimRight(buf.Bytes(), "\n"), nil
}
