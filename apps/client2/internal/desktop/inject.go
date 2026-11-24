package desktop

import (
	_ "embed"
	"fmt"
	"os"
	"path/filepath"
	"respire-client2/internal/i18n"
	"runtime"
	"strings"
	"unicode"
)

// The desktop installer deploys the historical zh-CN prompt edition.
// Keep its content stable for existing installs and managed blocks.

//go:embed i18n/zh-CN/inject/respire.md
var instructionsFullMD string

//go:embed i18n/zh-CN/inject/respire-lite.md
var instructionsLiteMD string

//go:embed i18n/zh-CN/inject/respire-readonly.md
var instructionsReadonlyMD string

//go:embed i18n/zh-CN/inject/respire-off.md
var instructionsOffMD string

//go:embed i18n/zh-CN/inject/respire-workbuddy-ref.md
var workbuddyRefTemplate string

// instructionsMD picks the inject source from workspace mode: off uses the off
// edition, readonly the read-only edition, otherwise the lite edition (same
// mapping as the CLI's instructions_md()).
func instructionsMD(mode string) string {
	switch mode {
	case "off":
		return instructionsOffMD
	case "readonly":
		return instructionsReadonlyMD
	default:
		return instructionsLiteMD
	}
}

// Inject-block markers (block mode: bound user content vs injected content).
const (
	blockBegin       = "<!-- respire:begin -->"
	blockEnd         = "<!-- respire:end -->"
	legacyBlockBegin = "<!-- 1memory:begin -->"
	legacyBlockEnd   = "<!-- 1memory:end -->"
)

// injectHome carries the paths the inject code needs; tests point it at a
// temporary directory instead of the real user profile.
type injectHome struct {
	home      string // os.UserHomeDir()
	localData string // %LOCALAPPDATA% (Windows entity file root)
	mode      string // workspace mode: normal | readonly | off
}

func (e injectHome) instructions() string { return instructionsMD(e.mode) }

// entityPath is the inject entity file: Windows `%LOCALAPPDATA%\respire\docs\respire.md`,
// Unix `~/.local/share/respire/docs/respire.md` (do not force an XDG path).
func (e injectHome) entityPath() string {
	if runtime.GOOS == "windows" {
		base := e.localData
		if base == "" {
			base = filepath.Join(e.home, "AppData", "Local")
		}
		return filepath.Join(base, "respire", "docs", "respire.md")
	}
	return filepath.Join(e.home, ".local", "share", "respire", "docs", "respire.md")
}

func expandHome(home, p string) string {
	if rest, ok := strings.CutPrefix(p, "~/"); ok {
		return filepath.Join(home, rest)
	}
	if rest, ok := strings.CutPrefix(p, `~\`); ok {
		return filepath.Join(home, rest)
	}
	return p
}

func joinHome(home string, elems ...string) string {
	return filepath.Join(append([]string{home}, elems...)...)
}

// workbuddyRefMD renders the workbuddy reference block: MEMORY.md is injected
// each turn as <user_memory> (4000-char cap), so it carries the mandatory
// gates only and points at the entity file for the full policy.
func workbuddyRefMD(entity string) string {
	return strings.ReplaceAll(workbuddyRefTemplate, "{entity}", entity)
}

// displayPath abbreviates the home prefix as `~` for display.
func displayPath(e injectHome, path string) string {
	if rest, err := filepath.Rel(e.home, path); err == nil && rest != ".." && !strings.HasPrefix(rest, ".."+string(filepath.Separator)) {
		return "~/" + filepath.ToSlash(rest)
	}
	return path
}

// injectTarget is one row of the install page's agent list.
type injectTarget struct {
	ID              string        `json:"id"`
	Name            string        `json:"name"`
	Path            string        `json:"path"`
	RealPath        string        `json:"realPath"`
	Mode            string        `json:"mode"` // inject | block | reference | ref-block | plugin
	LikelyInstalled bool          `json:"likelyInstalled"`
	State           string        `json:"state"` // fresh | stale | none | absent
	Plugin          *pluginStatus `json:"plugin,omitempty"`
}

// injectTargets probes every target (shown in the UI before inject).
func injectTargets(e injectHome) []injectTarget {
	dirAt := func(elem ...string) bool {
		info, err := os.Stat(joinHome(e.home, elem...))
		return err == nil && info.IsDir()
	}
	probe := []struct {
		id    string
		known bool
	}{
		{"generic", true},
		{"dsh", dirAt(".dsh")},
		{"opencode", dirAt(".config", "opencode")},
		{"codex", dirAt(".codex")},
		{"claude", dirAt(".claude")},
		{"codebuddy", dirAt(".codebuddy")},
		{"workbuddy", dirAt(".workbuddy")},
		{"kylinbot", dirAt(".kylinbot")},
		{"pi", dirAt(".pi")},
		{"zcode", dirAt(".zcode") || dirAt(".zigcode")},
		{"deepseek", dirAt(".deepseek")},
		{"qwen", dirAt(".qwen") || dirAt(".config", "qwen")},
		{"doubao", dirAt(".doubao") || dirAt(".config", "doubao")},
		{"grok", dirAt(".grok")},
	}
	out := make([]injectTarget, 0, len(probe))
	for _, item := range probe {
		target := injectTarget{ID: item.id, LikelyInstalled: item.known}
		switch item.id {
		case "dsh":
			target.Name, target.Path, target.Mode = "dsh", "~/.dsh/AGENTS.md", "inject"
		case "opencode":
			target.Name, target.Path, target.Mode = "opencode", displayPath(e, e.entityPath()), "reference"
		case "codex":
			target.Name, target.Path, target.Mode = "codex", "~/.codex/AGENTS.md", "block"
		case "claude":
			target.Name, target.Path, target.Mode = "claude code", "~/.claude/CLAUDE.md", "block"
		case "codebuddy":
			target.Name, target.Path, target.Mode = "codebuddy", "~/.codebuddy/AGENTS.md", "block"
		case "workbuddy":
			target.Name, target.Path, target.Mode = "workbuddy", "~/.workbuddy/MEMORY.md", "ref-block"
		case "pi":
			target.Name, target.Path, target.Mode = "pi agent", "~/.pi/AGENTS.md", "block"
		case "zcode":
			target.Name, target.Path, target.Mode = "ZCode", "~/.zcode/AGENTS.md", "block"
		case "deepseek":
			target.Name, target.Path, target.Mode = "deepseek harness", "~/.deepseek/AGENTS.md", "block"
		case "qwen":
			target.Name, target.Path, target.Mode = "qwen assistant", "~/.qwen/AGENTS.md", "block"
		case "doubao":
			target.Name, target.Path, target.Mode = "doubao office", "~/.doubao/AGENTS.md", "block"
		case "grok":
			target.Name, target.Path, target.Mode = "grok build", "~/.grok/AGENTS.md", "block"
		case "kylinbot":
			target.Name, target.Path, target.Mode = "kylinbot", "~/.kylinbot/workspace/AGENTS.md", "block"
		default:
			target.Name, target.Path, target.Mode = "generic AGENTS.md", "~/AGENTS.md", "block"
		}
		if target.Mode != "reference" {
			target.RealPath = expandHome(e.home, target.Path)
		} else {
			target.RealPath = e.entityPath()
		}
		target.State = e.targetState(&target)
		out = append(out, target)
	}
	return out
}

// targetState computes one target's state; the workbuddy reference block also
// requires the entity file to be current.
func (e injectHome) targetState(target *injectTarget) string {
	if target.ID == "opencode" {
		return e.opencodeState()
	}
	expected := e.instructions()
	if target.Mode == "ref-block" {
		expected = workbuddyRefMD(e.entityPath())
	}
	state := checkState(target.RealPath, target.Mode, expected)
	if target.Mode == "ref-block" && state == "fresh" &&
		checkState(e.entityPath(), "reference", e.instructions()) != "fresh" {
		return "stale"
	}
	return state
}

// checkState: absent (no file) / fresh (up to date) / stale (injected but not
// current) / none (file exists, not injected).
func checkState(path, mode, expected string) string {
	text, err := os.ReadFile(path)
	if err != nil {
		return "absent"
	}
	s := string(text)
	switch mode {
	// Cover/reference: the target file IS the inject source; compare the whole text
	case "inject", "reference":
		if trimSpaceRight(s) == trimSpaceRight(expected) {
			return "fresh"
		}
		return "stale"
	// Block: no markers → not injected; markers present → compare inner content
	default:
		if strings.Contains(s, legacyBlockBegin) || strings.Contains(s, legacyBlockEnd) {
			return "stale"
		}
		inner, ok := extractBlock(s)
		if !ok {
			return "none"
		}
		if inner == strings.ReplaceAll(trimSpaceRight(expected), "\r\n", "\n") {
			return "fresh"
		}
		return "stale"
	}
}

func trimSpaceRight(s string) string { return strings.TrimRightFunc(s, unicode.IsSpace) }

func extractBlock(text string) (string, bool) {
	start := strings.Index(text, blockBegin)
	if start < 0 {
		return "", false
	}
	start += len(blockBegin)
	end := strings.Index(text, blockEnd)
	if end < start {
		return "", false
	}
	return strings.TrimFunc(text[start:end], unicode.IsSpace), true
}

// injectOne writes one target. Returns whether a write actually happened.
func injectOne(e injectHome, id string) (bool, error) {
	switch id {
	case "dsh":
		return writeCover(e, joinHome(e.home, ".dsh", "AGENTS.md"))
	case "opencode":
		return injectOpencode(e)
	case "codex":
		return writeBlock(e, joinHome(e.home, ".codex", "AGENTS.md"))
	case "claude":
		return writeBlock(e, joinHome(e.home, ".claude", "CLAUDE.md"))
	case "codebuddy":
		return writeBlock(e, joinHome(e.home, ".codebuddy", "AGENTS.md"))
	case "workbuddy":
		return injectWorkbuddy(e)
	case "pi":
		return writeBlock(e, joinHome(e.home, ".pi", "AGENTS.md"))
	case "zcode":
		return writeBlock(e, joinHome(e.home, ".zcode", "AGENTS.md"))
	case "deepseek":
		return writeBlock(e, joinHome(e.home, ".deepseek", "AGENTS.md"))
	case "qwen":
		return writeBlock(e, joinHome(e.home, ".qwen", "AGENTS.md"))
	case "doubao":
		return writeBlock(e, joinHome(e.home, ".doubao", "AGENTS.md"))
	case "grok":
		return writeBlock(e, joinHome(e.home, ".grok", "AGENTS.md"))
	case "kylinbot":
		return writeBlock(e, joinHome(e.home, ".kylinbot", "workspace", "AGENTS.md"))
	case "generic":
		return writeBlock(e, joinHome(e.home, "AGENTS.md"))
	default:
		return false, fmt.Errorf("unknown inject target: %s", id)
	}
}

// removeOne uninstalls one target. Returns whether a write actually happened.
func removeOne(e injectHome, id string) (bool, error) {
	switch id {
	case "dsh":
		return removeCover(e, joinHome(e.home, ".dsh", "AGENTS.md"))
	case "opencode":
		return removeOpencode(e)
	case "codex":
		return removeBlock(e, joinHome(e.home, ".codex", "AGENTS.md"))
	case "claude":
		return removeBlock(e, joinHome(e.home, ".claude", "CLAUDE.md"))
	case "codebuddy":
		return removeBlock(e, joinHome(e.home, ".codebuddy", "AGENTS.md"))
	case "workbuddy":
		return removeBlock(e, joinHome(e.home, ".workbuddy", "MEMORY.md"))
	case "pi":
		return removeBlock(e, joinHome(e.home, ".pi", "AGENTS.md"))
	case "zcode":
		return removeBlock(e, joinHome(e.home, ".zcode", "AGENTS.md"))
	case "deepseek":
		return removeBlock(e, joinHome(e.home, ".deepseek", "AGENTS.md"))
	case "qwen":
		return removeBlock(e, joinHome(e.home, ".qwen", "AGENTS.md"))
	case "doubao":
		return removeBlock(e, joinHome(e.home, ".doubao", "AGENTS.md"))
	case "grok":
		return removeBlock(e, joinHome(e.home, ".grok", "AGENTS.md"))
	case "kylinbot":
		return removeBlock(e, joinHome(e.home, ".kylinbot", "workspace", "AGENTS.md"))
	case "generic":
		return removeBlock(e, joinHome(e.home, "AGENTS.md"))
	default:
		return false, fmt.Errorf("unknown inject target: %s", id)
	}
}

func writeCover(e injectHome, path string) (bool, error) {
	if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
		return false, err
	}
	// Drop a symlink first — os.WriteFile would follow it and overwrite the repo source
	if info, err := os.Lstat(path); err == nil && info.Mode()&os.ModeSymlink != 0 {
		if err := os.Remove(path); err != nil {
			return false, err
		}
	}
	source := e.instructions()
	if old, err := os.ReadFile(path); err == nil && string(old) == source {
		return false, nil
	}
	if err := os.WriteFile(path, []byte(source), 0o644); err != nil {
		return false, err
	}
	return true, nil
}

func removeCover(e injectHome, path string) (bool, error) {
	text, err := os.ReadFile(path)
	if err != nil {
		// No file (or unreadable) → nothing of ours to remove
		return false, nil
	}
	// Identical to the inject source → treat as a pure inject file and delete it
	if string(text) == e.instructions() || string(text) == instructionsFullMD {
		return true, os.Remove(path)
	}
	// User-owned content → strip the block only
	return removeBlock(e, path)
}

func (e injectHome) opencodeState() string {
	dir := joinHome(e.home, ".config", "opencode")
	referenced := false
	for _, name := range []string{"opencode.json", "opencode.jsonc"} {
		text, err := os.ReadFile(joinPath(dir, name))
		if err != nil {
			continue // missing config is fine; unreadable ones just don't count
		}
		cfg, err := parseJSONObject(text)
		if err != nil {
			return "manual"
		}
		// A later jsonc overrides the former only when it declares instructions.
		if entries, ok := cfg["instructions"].([]any); ok {
			for _, entry := range entries {
				if raw, ok := entry.(string); ok && e.sameEntry(raw) {
					referenced = true
				}
			}
		}
	}
	if !referenced {
		return "none"
	}
	return checkState(e.entityPath(), "reference", e.instructions())
}

func (e injectHome) sameEntry(raw string) bool {
	return filepath.Clean(expandHome(e.home, raw)) == filepath.Clean(e.entityPath())
}

func injectOpencode(e injectHome) (bool, error) {
	changed, err := writeCover(e, e.entityPath())
	if err != nil {
		return false, err
	}
	// Both opencode.json and opencode.jsonc are loaded (jsonc later can override
	// instructions) — write both if present
	dir := joinHome(e.home, ".config", "opencode")
	for _, name := range []string{"opencode.json", "opencode.jsonc"} {
		path := joinPath(dir, name)
		if name == "opencode.jsonc" && !fileExists(path) {
			continue
		}
		item, err := patchInstructions(e, path, true)
		if err != nil {
			return false, err
		}
		changed = changed || item
	}
	// oh-my-openagent prompt_append (touch only if the field exists; idempotent)
	oma := joinPath(dir, "oh-my-openagent.json")
	if fileExists(oma) {
		item, err := patchPromptAppend(e, oma, true)
		if err != nil {
			return false, err
		}
		changed = changed || item
	}
	return changed, nil
}

func removeOpencode(e injectHome) (bool, error) {
	// The entity file is shared by OpenCode and WorkBuddy; uninstall only drops
	// this target's reference.
	changed := false
	dir := joinHome(e.home, ".config", "opencode")
	for _, name := range []string{"opencode.json", "opencode.jsonc"} {
		path := joinPath(dir, name)
		if fileExists(path) {
			item, err := patchInstructions(e, path, false)
			if err != nil {
				return false, err
			}
			changed = changed || item
		}
	}
	oma := joinPath(dir, "oh-my-openagent.json")
	if fileExists(oma) {
		item, err := patchPromptAppend(e, oma, false)
		if err != nil {
			return false, err
		}
		changed = changed || item
	}
	return changed, nil
}

// patchInstructions adds/removes the inject path in the instructions array;
// keeps other entries (the old impl replaced the whole list and deleted
// extras; fixed 2026-09-09 in the CLI).
func patchInstructions(e injectHome, path string, add bool) (bool, error) {
	entry := e.entityPath()
	textBytes, err := os.ReadFile(path)
	if err != nil {
		if !os.IsNotExist(err) {
			return false, err
		}
		if !add {
			return false, nil
		}
		cfg := map[string]any{
			"$schema":      "https://opencode.ai/config.json",
			"instructions": []any{entry},
		}
		data, err := jsonPretty(cfg)
		if err != nil {
			return false, err
		}
		if err := os.MkdirAll(filepath.Dir(path), 0o755); err != nil {
			return false, err
		}
		return true, os.WriteFile(path, data, 0o644)
	}
	text := string(textBytes)
	cfg, err := parseJSONObject(textBytes)
	if err != nil {
		return false, fmt.Errorf(i18n.Messaged45cf70fc22fdf6c, path, entry, err)
	}
	switch existing := cfg["instructions"].(type) {
	case []any:
		// Normalize: `~` form and absolute form are the same item; drop every
		// form then add the canonical one back (no duplicates)
		kept := existing[:0:0]
		for _, item := range existing {
			if raw, ok := item.(string); ok && e.sameEntry(raw) {
				continue
			}
			kept = append(kept, item)
		}
		if add {
			kept = append([]any{entry}, kept...)
		}
		cfg["instructions"] = kept
	case nil:
		if add {
			cfg["instructions"] = []any{entry}
		}
	default:
		return false, fmt.Errorf(i18n.Message12ea74fc44a68fbd, path)
	}
	data, err := jsonPretty(cfg)
	if err != nil {
		return false, err
	}
	if string(data) == text {
		return false, nil
	}
	return true, os.WriteFile(path, data, 0o644)
}
