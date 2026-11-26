package desktop

import (
	"bytes"
	"crypto/sha256"
	"encoding/json"
	"fmt"
	"io/fs"
	"os"
	"os/exec"
	"path/filepath"
	"regexp"
	"strings"
	"testing"
)

const webUIPath = "apps/client2/internal/desktop/frontend"

type webUIFile struct {
	Size   int    `json:"size"`
	SHA256 string `json:"sha256"`
}

type webUIManifest struct {
	Schema int                  `json:"schema"`
	Source string               `json:"source_sha"`
	Files  map[string]webUIFile `json:"files"`
}

func frontendAssetSnapshot(t *testing.T, source string) (webUIManifest, map[string][]byte) {
	t.Helper()
	git := func(args ...string) []byte {
		t.Helper()
		out, err := exec.Command("git", args...).Output()
		if err != nil {
			t.Fatalf("git %s: %v", args[0], err)
		}
		return out
	}
	if !regexp.MustCompile(`^[a-f0-9]{40}$`).MatchString(source) ||
		strings.TrimSpace(string(git("rev-parse", "HEAD"))) != source {
		t.Fatal("Expected the exact reviewed source SHA")
	}
	clean := git("status", "--porcelain", "--untracked-files=no")
	if len(clean) != 0 {
		t.Fatal("Tracked checkout must be clean")
	}
	prefix := webUIPath + "/"
	root := strings.TrimSpace(string(git("rev-parse", "--show-toplevel")))
	tracked := bytes.Split(git("-C", root, "ls-tree", "-r", "-z", "--name-only", source, "--", webUIPath), []byte{0})
	manifest := webUIManifest{Schema: 1, Source: source, Files: map[string]webUIFile{}}
	assets := map[string][]byte{}
	for _, name := range tracked[:len(tracked)-1] {
		path, ok := strings.CutPrefix(string(name), prefix)
		if !ok || !fs.ValidPath(path) || path == "." || strings.ContainsAny(path, "\\:") {
			t.Fatal("Invalid asset path in reviewed tree")
		}
		embedded, err := frontend.ReadFile("frontend/" + path)
		if err != nil {
			t.Fatal("Reviewed asset missing from production embed", err)
		}
		if !bytes.Equal(embedded, git("-C", root, "show", source+":"+string(name))) {
			t.Fatal("Embedded asset bytes differ from the reviewed revision")
		}
		assets[path] = embedded
		manifest.Files[path] = webUIFile{Size: len(embedded), SHA256: fmt.Sprintf("%x", sha256.Sum256(embedded))}
	}
	err := fs.WalkDir(frontend, "frontend", func(name string, entry fs.DirEntry, err error) error {
		if err != nil || entry.IsDir() {
			return err
		}
		if _, ok := assets[strings.TrimPrefix(name, "frontend/")]; !ok || !entry.Type().IsRegular() {
			return fmt.Errorf("production embed contains an extra or nonregular asset")
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
	for _, name := range []string{"workspace.html", "quickpanel.html", "ui/i18n.js"} {
		if _, ok := assets[name]; !ok {
			t.Fatal("Missing a required product entry asset")
		}
	}
	return manifest, assets
}

// TestExportEmbeddedWebUI is an opt-in test tool, not a product CLI flag.
// It invokes no native launcher or Runtime, and copies the same embed.FS that
// the production Wails server uses, rejecting source, inclusion or byte drift.
func TestExportEmbeddedWebUI(t *testing.T) {
	output := os.Getenv("RESPIRE_E2E_EXPORT_WEBUI_DIR")
	if output == "" {
		t.Skip("No explicit WebUI fixture output requested")
	}
	if !filepath.IsAbs(output) || filepath.Clean(output) != output {
		t.Fatal("Expected a fresh, absolute WebUI output directory")
	}
	parent := filepath.Dir(output)
	if canonical, err := filepath.EvalSymlinks(parent); err != nil || canonical != parent {
		t.Fatal("WebUI output parent must exist and not redirect")
	}
	manifest, assets := frontendAssetSnapshot(t, os.Getenv("RESPIRE_E2E_PRODUCT_SHA"))
	if err := os.Mkdir(output, 0700); err != nil {
		t.Fatal("WebUI output directory must not exist:", err)
	}
	for name, content := range assets {
		path := filepath.Join(output, "assets", filepath.FromSlash(name))
		if err := os.MkdirAll(filepath.Dir(path), 0700); err != nil {
			t.Fatal(err)
		}
		if err := os.WriteFile(path, content, 0600); err != nil {
			t.Fatal(err)
		}
	}
	data, err := json.MarshalIndent(manifest, "", "  ")
	if err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(filepath.Join(output, "webui.json"), append(data, '\n'), 0600); err != nil {
		t.Fatal(err)
	}
}
