package desktop

import (
	"bytes"
	"encoding/json"
	"io/fs"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"regexp"
	"strings"
	"testing"

	"github.com/wailsapp/wails/v3/pkg/application"
	"respire-client2/internal/host"
)

func TestPreferencesHTTPRoundTripAllFields(t *testing.T) {
	t.Setenv("RSRS_DATA_DIR", t.TempDir())
	t.Setenv("RSRS_RPC_PORT", "15169")
	t.Setenv("RSRS_RPC_TOKEN", "fixture-token")
	manager, err := host.New()
	if err != nil {
		t.Fatal(err)
	}
	upstream := httptest.NewServer(http.NotFoundHandler())
	t.Cleanup(upstream.Close)
	manager.Endpoint = upstream.URL
	d := &desktop{m: manager, app: application.New(application.Options{Name: "Respire Client2 Preferences Test"})}
	handler := d.handler(http.NotFoundHandler())
	request := func(method, path, body string, wantOK bool) json.RawMessage {
		t.Helper()
		req := httptest.NewRequest(method, path, strings.NewReader(body))
		req.Header.Set("X-Respire-Client", "1")
		req.Header.Set("Content-Type", "application/json")
		response := httptest.NewRecorder()
		handler.ServeHTTP(response, req)
		var reply struct {
			OK     bool            `json:"ok"`
			Result json.RawMessage `json:"result"`
			Error  string          `json:"error"`
		}
		if err := json.Unmarshal(response.Body.Bytes(), &reply); err != nil {
			t.Fatalf("%s %s: %v", method, path, err)
		}
		if reply.OK != wantOK || (wantOK && response.Code != http.StatusOK) || (!wantOK && reply.Error == "") {
			t.Fatalf("%s %s %s: got %d %s", method, path, body, response.Code, response.Body)
		}
		return reply.Result
	}
	readFile := func() []byte {
		t.Helper()
		data, err := os.ReadFile(filepath.Join(manager.Root, "preferences.json"))
		if err != nil {
			t.Fatal(err)
		}
		return data
	}
	assertState := func(want host.Preferences) {
		t.Helper()
		var state struct {
			Preferences host.Preferences `json:"preferences"`
		}
		if err := json.Unmarshal(request(http.MethodGet, "/api/state", "", true), &state); err != nil {
			t.Fatal(err)
		}
		if state.Preferences != want {
			t.Fatalf("GET preferences = %+v, want %+v", state.Preferences, want)
		}
		var persisted host.Preferences
		if err := json.Unmarshal(readFile(), &persisted); err != nil {
			t.Fatal(err)
		}
		if persisted != want {
			t.Fatalf("persisted preferences = %+v, want %+v", persisted, want)
		}
	}
	const fullPatch = `{
		"autoRuntime":false,"autoUpdate":false,"runtimeChannel":"stable","clientChannel":"dev",
		"layout":"compact","theme":"dark","language":"en","proxyMode":"direct",
		"modelMirror":" https://HF-MIRROR.COM/ ","clientUpdateInterval":"5m","runtimeUpdateInterval":"1h",
		"clientAutoUpdate":false,"clientAutoRestart":false,"runtimeAutoUpdate":false,"runtimeAutoRestart":false,
		"sidebarWidth":242,"memoryIndexWidth":263,"onboardingVersion":1,"onboardingCompleted":true,
		"onboardingStep":"done","onboardingSyncChoice":"offline"
	}`
	var fields map[string]json.RawMessage
	if err := json.Unmarshal([]byte(fullPatch), &fields); err != nil {
		t.Fatal(err)
	}
	for _, key := range preferencePatchKeys {
		if _, ok := fields[key]; !ok {
			t.Fatalf("full patch omits %q", key)
		}
	}
	var want host.Preferences
	if err := json.Unmarshal([]byte(fullPatch), &want); err != nil {
		t.Fatal(err)
	}
	request(http.MethodPost, "/api/preferences", fullPatch, true)
	want.ModelMirror = "https://hf-mirror.com"
	assertState(want)
	request(http.MethodPost, "/api/preferences", `{"modelMirror":"  "}`, true)
	want.ModelMirror = "auto"
	assertState(want)
	request(http.MethodPost, "/api/preferences", `{"modelMirror":"https://mirror.example.org:8443/"}`, true)
	want.ModelMirror = "https://mirror.example.org:8443"
	assertState(want)
	before := readFile()
	for _, rejected := range []string{
		`{"unknown":true}`, `{"theme":"light","modelMirror":"ftp://bad"}`, `{"modelMirror":"https://"}`,
		`{"modelMirror":"https://example.com/path"}`, `{"modelMirror":"https://example.com/?query=1"}`,
		`{"modelMirror":"http://:8443"}`, `{"modelMirror":"https://exa mple.org"}`,
		`{"modelMirror":"https://user@example.com"}`, `{"modelMirror":"https://example.com/#fragment"}`,
		`{"modelMirror":123}`, `{"modelMirror":"https://example.com"} {}`,
	} {
		request(http.MethodPost, "/api/preferences", rejected, false)
		assertState(want)
		if !bytes.Equal(readFile(), before) {
			t.Fatalf("rejected patch rewrote preferences: %s", rejected)
		}
	}
}

func TestLegacyModelMirrorMigration(t *testing.T) {
	for raw, want := range map[string]string{
		"": "auto", "bad mirror": "auto", "javascript:alert(1)": "auto",
		" HF-MIRROR.NET/ ": "https://hf-mirror.net", "HTTP://mirror.example.org:8443///": "http://mirror.example.org:8443",
	} {
		got := host.MigratePreferences(host.Preferences{ModelMirror: raw}, nil).ModelMirror
		if got != want {
			t.Errorf("migrate mirror %q = %q, want %q", raw, got, want)
		}
	}
}

func TestFrontendPreferenceKeysMatchContract(t *testing.T) {
	keys := make(map[string]bool)
	for _, key := range preferencePatchKeys {
		keys[key] = true
	}
	attributes := regexp.MustCompile(`\bdata-pref\s*=\s*\\?["']([^\r\n\\"']+)\\?["']`)
	calls := regexp.MustCompile(`\bsavePreferences\s*\(\s*\{`)
	tokens := regexp.MustCompile("(?s)/\\*.*?\\*/|//[^\\n]*|\"(?:\\\\.|[^\"\\\\])*\"|'(?:\\\\.|[^'\\\\])*'|`(?:\\\\.|[^`\\\\])*`|[a-zA-Z_$][\\w$]*|[^\\s]")
	err := filepath.WalkDir("frontend", func(path string, entry fs.DirEntry, err error) error {
		if err != nil || entry.IsDir() || filepath.Ext(path) != ".js" {
			return err
		}
		data, err := os.ReadFile(path)
		if err != nil {
			return err
		}
		check := func(key string, offset int) {
			if !keys[key] {
				t.Errorf("%s:%d: unknown frontend preference key %q", path, bytes.Count(data[:offset], []byte("\n"))+1, key)
			}
		}
		for _, match := range attributes.FindAllSubmatchIndex(data, -1) {
			check(string(data[match[2]:match[3]]), match[2])
		}
		for _, call := range calls.FindAllIndex(data, -1) {
			depth, atKey := 1, true
			for _, loc := range tokens.FindAllIndex(data[call[1]:], -1) {
				start, end := call[1]+loc[0], call[1]+loc[1]
				token := string(data[start:end])
				if strings.HasPrefix(token, "//") || strings.HasPrefix(token, "/*") {
					continue
				}
				if depth == 1 && atKey {
					if token != "}" && token != "[" && token != "." {
						check(strings.Trim(token, `"'`), start)
					}
					atKey = false
				}
				switch token {
				case "{", "[", "(":
					depth++
				case "}", "]", ")":
					depth--
				case ",":
					atKey = depth == 1
				}
				if depth == 0 {
					break
				}
			}
		}
		return nil
	})
	if err != nil {
		t.Fatal(err)
	}
}

func TestPatchPreferencesKeepsUnsentFields(t *testing.T) {
	base := host.Preferences{}
	if err := json.Unmarshal([]byte(`{
		"autoRuntime": true, "autoUpdate": true,
		"runtimeChannel": "stable", "clientChannel": "stable",
		"layout": "sidebar", "theme": "dark", "language": "zh",
		"clientUpdateInterval": "6h", "runtimeUpdateInterval": "6h",
		"clientAutoUpdate": true, "clientAutoRestart": true,
		"runtimeAutoUpdate": true, "runtimeAutoRestart": true,
		"sidebarWidth": 240, "memoryIndexWidth": 260,
		"onboardingVersion": 1, "onboardingCompleted": false,
		"onboardingStep": "model", "onboardingSyncChoice": "cloud"
	}`), &base); err != nil {
		t.Fatal(err)
	}
	// A legacy settings form posts only the fields it renders.
	patched, err := patchPreferences(base, []byte(`{"theme":"light","layout":"compact"}`))
	if err != nil {
		t.Fatal(err)
	}
	if patched.Theme != "light" || patched.Layout != "compact" {
		t.Fatalf("patched fields must apply, got %+v", patched)
	}
	if !patched.RuntimeAutoRestart || !patched.RuntimeAutoUpdate {
		t.Fatalf("runtimeAutoRestart/runtimeAutoUpdate must survive the patch, got %+v", patched)
	}
	if patched.OnboardingStep != "model" || patched.OnboardingSyncChoice != "cloud" || !patched.ClientAutoUpdate {
		t.Fatalf("unrelated fields must survive the patch, got %+v", patched)
	}
}

func TestPatchPreferencesAppliesOnboardingFields(t *testing.T) {
	base := host.Preferences{OnboardingVersion: 1, OnboardingStep: "language"}
	patched, err := patchPreferences(base, []byte(`{"onboardingCompleted":true,"onboardingStep":"done","onboardingSyncChoice":"offline"}`))
	if err != nil {
		t.Fatal(err)
	}
	if !patched.OnboardingCompleted || patched.OnboardingStep != "done" || patched.OnboardingSyncChoice != "offline" {
		t.Fatalf("onboarding patch failed, got %+v", patched)
	}
	if patched.OnboardingVersion != 1 || patched.OnboardingStep == "" {
		t.Fatalf("base fields must persist, got %+v", patched)
	}
}

func TestPatchPreferencesRejectsGarbage(t *testing.T) {
	if _, err := patchPreferences(host.Preferences{}, []byte(`{"sidebarWidth":"wide"}`)); err == nil {
		t.Fatal("type mismatch must fail")
	}
	if _, err := patchPreferences(host.Preferences{}, []byte(`not json`)); err == nil {
		t.Fatal("malformed body must fail")
	}
}
