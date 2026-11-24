package desktop

import "respire-client2/internal/i18n"

import (
	"bytes"
	"context"
	"fmt"
	"os"
	"os/exec"
	"path/filepath"
	"runtime"
	"strings"
)

type pluginCommand struct {
	name string
	args []string
	dir  string
	env  []string
}

type pluginOutput struct {
	bytes.Buffer
	overflow bool
}

func (b *pluginOutput) Write(data []byte) (int, error) {
	if b.Len()+len(data) > 64<<10 {
		b.overflow = true
		data = data[:max(0, 64<<10-b.Len())]
	}
	return b.Buffer.Write(data)
}

func (b *pluginOutput) String() string {
	if b.overflow {
		return b.Buffer.String() + i18n.Messagecf7b89e4d001b98f
	}
	return b.Buffer.String()
}

type pluginStatus struct {
	ID       string `json:"id"`
	Name     string `json:"name"`
	Repo     string `json:"repo"`
	Source   bool   `json:"source"`
	Command  bool   `json:"command"`
	Path     string `json:"path"`
	Auto     bool   `json:"auto"`
	Manual   string `json:"manual"`
	Profiles string `json:"profiles"`
}

func pluginAuto(id string) bool {
	switch id {
	case "respire-zcode", "respire-kimi":
		return false
	default:
		return true
	}
}

type pluginSpec struct {
	name    string
	repo    string
	command string
	manual  string
}

var pluginSpecs = []pluginSpec{
	{"magpie", "respire-magpie", "magpie", i18n.Message53c3946d74d5cbfd},
	{"dsh", "respire-dsh", "dsh", i18n.Message379dba797814031b},
	{"workbuddy", "respire-workbuddy", "bash", i18n.Message35895fe86cd21d9f},
	{"codex", "respire-codex", "codex", i18n.Message0f073a1bcb76a3bf},
	{"qoder", "respire-qoder", "qoder", i18n.Message5cf92c318fc70e6e},
	{"hermes", "respire-hermes", "bash", i18n.Message2204128f104cb9e3},
	{"zcode", "respire-zcode", "", i18n.Messagef07d647fa58013c2},
	{"dimcode", "respire-dimcode", "dim", i18n.Message174507411f7d332d},
	{"pi", "respire-pi", "pi", i18n.Message306174e5a248bf84},
	{"kimi", "respire-kimi", "", i18n.Messagebfbe169152d2b765},
}

func pluginRepo(id string, mirror bool) string {
	url := "https://github.com/risense-ai/" + id
	if mirror {
		url = "https://gh-proxy.com/" + url
	}
	return url
}

func pluginByID(id string) (pluginSpec, bool) {
	for _, item := range pluginSpecs {
		if item.repo == id {
			return item, true
		}
	}
	return pluginSpec{}, false
}

func (d *desktop) pluginPath(repo string) string {
	return filepath.Join(d.m.Root, "plugins", repo)
}

func (d *desktop) PluginStatuses(ctx context.Context) []pluginStatus {
	result := make([]pluginStatus, 0, len(pluginSpecs))
	for _, spec := range pluginSpecs {
		item := pluginStatus{
			ID:     spec.repo,
			Name:   spec.name,
			Repo:   "https://github.com/risense-ai/" + spec.repo,
			Path:   d.pluginPath(spec.repo),
			Source: pluginSourceExists(d.pluginPath(spec.repo)),
			Auto:   pluginAuto(spec.repo),
			Manual: spec.manual,
		}
		if spec.command != "" {
			path, err := exec.LookPath(spec.command)
			item.Command = err == nil && path != ""
		}
		if spec.repo == "respire-dsh" {
			item.Profiles = "desktop"
		}
		result = append(result, item)
	}
	return result
}

func (d *desktop) InstallPlugin(ctx context.Context, id string, mirror bool) (string, error) {
	message, _, err := d.ensurePlugin(ctx, id, mirror)
	return message, err
}

// ensurePlugin prepares the local plugin source and, when the host CLI allows
// it, runs the install commands. soft reports "source ready, finish in the
// host UI" as success instead of an error (interface-installed hosts).
func (d *desktop) ensurePlugin(ctx context.Context, id string, mirror bool) (string, bool, error) {
	spec, ok := pluginByID(id)
	if !ok {
		return "", false, fmt.Errorf(i18n.Message560eedd354f60c9a)
	}
	path := d.pluginPath(id)
	if err := d.updatePluginSource(ctx, path, pluginRepo(id, mirror)); err != nil {
		return "", false, err
	}
	if !pluginAuto(id) {
		return fmt.Sprintf(i18n.Message4df0422d8260574d, spec.name, path), true, nil
	}
	commands, err := d.pluginCommands(spec, path)
	if err != nil {
		return "", false, err
	}
	var out pluginOutput
	env, err := d.m.ProxyEnv(os.Environ())
	if err != nil {
		return "", false, err
	}
	for _, command := range commands {
		command.env = env
		if err = runPluginCommand(ctx, command, &out); err != nil {
			return "", false, err
		}
	}
	message := i18n.Messagec6e8b23632456876
	if text := strings.TrimSpace(out.String()); text != "" {
		message += i18n.Message9540f928412c913d + text
	}
	return message, false, nil
}

func (d *desktop) pluginCommands(spec pluginSpec, path string) ([]pluginCommand, error) {
	switch spec.repo {
	case "respire-magpie":
		return []pluginCommand{{name: spec.command, args: []string{"plugin", "add", path}}}, nil
	case "respire-dsh":
		return []pluginCommand{{name: spec.command, args: []string{"plugin", "--profile", "desktop", "add", "@rsrsai/dsh"}}}, nil
	case "respire-workbuddy":
		return []pluginCommand{{name: "bash", args: []string{"install.sh"}, dir: path}}, nil
	case "respire-codex":
		return []pluginCommand{
			{name: spec.command, args: []string{"plugin", "marketplace", "add", "risense-ai/respire-codex", "--ref", "main"}},
			{name: spec.command, args: []string{"plugin", "add", "respire@rsrsai"}},
		}, nil
	case "respire-qoder":
		return []pluginCommand{
			{name: spec.command, args: []string{"plugins", "marketplace", "add", path}},
			{name: spec.command, args: []string{"plugins", "install", "respire-qoder@risense"}},
		}, nil
	case "respire-hermes":
		return []pluginCommand{{name: "bash", args: []string{"scripts/install.sh"}, dir: path}}, nil
	case "respire-dimcode":
		return []pluginCommand{
			{name: spec.command, args: []string{"plugin", "marketplace", "add", filepath.Join(path, ".agents", "plugins", "marketplace.json")}},
			{name: spec.command, args: []string{"plugin", "install", "respire@risense"}},
		}, nil
	case "respire-pi":
		return []pluginCommand{{name: spec.command, args: []string{"install", "npm:@rsrsai/pi"}}}, nil
	default:
		return nil, fmt.Errorf(i18n.Message4df0422d8260574d, spec.name, path)
	}
}

func runPluginCommand(ctx context.Context, command pluginCommand, out *pluginOutput) error {
	exe, err := exec.LookPath(command.name)
	if err != nil {
		return fmt.Errorf(i18n.Message3a7d4f42336c6016, command.name)
	}
	var cmd *exec.Cmd
	if runtime.GOOS == "windows" {
		extension := strings.ToLower(filepath.Ext(exe))
		if extension == ".cmd" || extension == ".bat" {
			cmd = exec.CommandContext(ctx, "cmd", "/C", exe)
			cmd.Args = append(cmd.Args, command.args...)
		}
	}
	if cmd == nil {
		cmd = exec.CommandContext(ctx, exe, command.args...)
	}
	cmd.Dir = command.dir
	if len(command.env) > 0 {
		cmd.Env = command.env
	}
	if out != nil {
		cmd.Stdout = out
		cmd.Stderr = out
	}
	if err = cmd.Run(); err != nil {
		if out != nil && out.Len() > 0 {
			text := strings.TrimSpace(out.String())
			if text != "" {
				return fmt.Errorf(i18n.Message37b44bf6b174626e, err, text)
			}
		}
		return err
	}
	return nil
}

func pluginSourceExists(path string) bool {
	_, err := os.Stat(filepath.Join(path, ".git"))
	return err == nil
}

func (d *desktop) updatePluginSource(ctx context.Context, path, repo string) error {
	env, err := d.m.ProxyEnv(os.Environ())
	if err != nil {
		return err
	}
	if pluginSourceExists(path) {
		if err := runPluginCommand(ctx, pluginCommand{name: "git", args: []string{"fetch", "--depth", "1", "origin", "main"}, dir: path, env: env}, nil); err != nil {
			return fmt.Errorf(i18n.Messageb32326a153c42891, err)
		}
		if err := runPluginCommand(ctx, pluginCommand{name: "git", args: []string{"merge", "--ff-only", "FETCH_HEAD"}, dir: path, env: env}, nil); err != nil {
			return fmt.Errorf(i18n.Messageed2d7d77254c9c0e, err)
		}
		return nil
	}
	return runPluginCommand(ctx, pluginCommand{name: "git", args: []string{"clone", "--depth", "1", "--branch", "main", repo, path}, env: env}, nil)
}
