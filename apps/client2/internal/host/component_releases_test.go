package host

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strings"
	"testing"
)

const componentCatalogTestURL = "https://api.github.com/repos/risense-ai/respire/releases?per_page=100&page=1"

func componentTestResponse(status int, body string) *http.Response {
	return &http.Response{StatusCode: status, Body: io.NopCloser(strings.NewReader(body)), Header: make(http.Header)}
}

func TestComponentRuntimeDoesNotFallbackAfterPrimaryFailures(t *testing.T) {
	target, err := runtimeTarget()
	if err != nil {
		t.Fatal(err)
	}
	name := "runtime-build-" + target + ".json"
	canonical := "https://github.com/risense-ai/respire/releases/download/runtime-v2.0.0/" + name
	for _, scenario := range []string{"auth", "network", "invalid-catalog", "missing-asset", "foreign-asset", "metadata-schema", "metadata-hash"} {
		t.Run(scenario, func(t *testing.T) {
			client := &http.Client{Transport: cliMetadataTransport(func(req *http.Request) (*http.Response, error) {
				switch req.URL.String() {
				case componentCatalogTestURL:
					switch scenario {
					case "auth":
						return componentTestResponse(403, "{}"), nil
					case "network":
						return nil, errors.New("synthetic transport error")
					case "invalid-catalog":
						return componentTestResponse(200, "null"), nil
					}
					assets := []ReleaseAsset{{Name: name, URL: canonical}}
					if scenario == "missing-asset" {
						assets = nil
					}
					if scenario == "foreign-asset" {
						assets[0].URL = strings.Replace(canonical, "/respire/", "/respire-runtime/", 1)
					}
					list, _ := json.Marshal([]any{map[string]any{
						"tag_name": "runtime-v2.0.0", "published_at": "2026-11-25T00:00:00Z", "draft": false, "prerelease": false, "assets": assets,
					}})
					return componentTestResponse(200, string(list)), nil
				case canonical:
					if scenario == "metadata-hash" {
						raw, _ := json.Marshal(RuntimeMetadata{Schema: 2, RuntimeVersion: "2.0.0", Tag: "v2.0.0", Target: target,
							Binary: "rsrs-runtime-" + target + binaryExtension(), BinaryHash: "bad",
							ArchiveFile: runtimeArchive(target), ArchiveHash: strings.Repeat("a", 64)})
						return componentTestResponse(200, string(raw)), nil
					}
					return componentTestResponse(200, "{"), nil
				default:
					t.Fatalf("unexpected fallback to %s", req.URL)
					return nil, nil
				}
			})}
			m := &Manager{HTTPClient: client, prefs: defaultPreferences()}
			m.prefs.RuntimeChannel = "stable"
			if _, _, err := m.LatestRuntime(context.Background()); err == nil {
				t.Fatal("invalid primary unexpectedly accepted")
			}
		})
	}
}

func TestComponentRuntimeBindsMetadataAndArchiveToSameRelease(t *testing.T) {
	target, err := runtimeTarget()
	if err != nil {
		t.Fatal(err)
	}
	prefix := "https://github.com/risense-ai/respire/releases/download/runtime-v2.0.0/"
	name := "runtime-build-" + target + ".json"
	binary := "rsrs-runtime-" + target + binaryExtension()
	archive := runtimeArchive(target)
	m := &Manager{prefs: defaultPreferences()}
	m.prefs.RuntimeChannel = "stable"
	m.HTTPClient = &http.Client{Transport: cliMetadataTransport(func(req *http.Request) (*http.Response, error) {
		switch req.URL.String() {
		case componentCatalogTestURL:
			raw, _ := json.Marshal([]any{
				map[string]any{"tag_name": "client2-v9.0.0", "published_at": "2026-11-25T00:00:00Z", "draft": false, "prerelease": false},
				map[string]any{"tag_name": "runtime-v2.0.0", "published_at": "2026-11-25T00:00:00Z", "draft": false, "prerelease": false,
					"assets": []ReleaseAsset{{name, prefix + name}, {archive, prefix + archive}, {binary, prefix + binary}}},
			})
			return componentTestResponse(200, string(raw)), nil
		case prefix + name:
			raw, _ := json.Marshal(RuntimeMetadata{Schema: 2, RuntimeVersion: "2.0.0", Tag: "v2.0.0", Target: target,
				Binary: binary, BinaryHash: strings.Repeat("a", 64), ArchiveFile: archive, ArchiveHash: strings.Repeat("b", 64)})
			return componentTestResponse(200, string(raw)), nil
		default:
			t.Fatalf("unexpected source %s", req.URL)
			return nil, nil
		}
	})}
	release, meta, err := m.LatestRuntime(context.Background())
	if err != nil || release.Tag != "runtime-v2.0.0" || meta.Tag != "v2.0.0" {
		t.Fatalf("selected release mismatch: %v", err)
	}
	for _, name := range []string{binary, archive} {
		if url, err := ReleaseAssetURL(release, runtimeRepo, name); err != nil || url != prefix+name {
			t.Fatal("artifact escaped selected source", err)
		}
	}
	if _, err := ReleaseAssetURL(release, cliRepo, binary); err == nil {
		t.Fatal("asset accepted for wrong component")
	}
}
