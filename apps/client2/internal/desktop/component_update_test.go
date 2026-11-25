package desktop

import (
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/base64"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"strings"
	"testing"

	"github.com/wailsapp/wails/v3/pkg/updater"
	"respire-client2/internal/host"
)

type updateTransport func(*http.Request) (*http.Response, error)

func (f updateTransport) RoundTrip(r *http.Request) (*http.Response, error) { return f(r) }

func TestComponentDesktopManifestAndArtifactStayOnOneRelease(t *testing.T) {
	binary := []byte("synthetic immutable installer")
	digest := sha256.Sum256(binary)
	name := host.ClientAsset("windows", "amd64")
	origin := "https://github.com/risense-ai/respire/releases/download/client2-v2.0.0/"
	catalogURL := "https://api.github.com/repos/risense-ai/respire/releases?per_page=100&page=1"
	for _, scenario := range []string{"valid", "catalog-error", "missing-manifest", "invalid-manifest", "foreign-asset", "hash-error", "partial"} {
		t.Run(scenario, func(t *testing.T) {
			client := &http.Client{Transport: updateTransport(func(req *http.Request) (*http.Response, error) {
				reply := func(status int, raw []byte) (*http.Response, error) {
					return &http.Response{StatusCode: status, Body: io.NopCloser(bytes.NewReader(raw))}, nil
				}
				switch req.URL.String() {
				case catalogURL:
					if scenario == "catalog-error" {
						return nil, errors.New("synthetic network failure")
					}
					assets := []host.ReleaseAsset{{Name: name, URL: origin + name}}
					if scenario != "missing-manifest" {
						assets = append(assets, host.ReleaseAsset{Name: updateManifestAsset, URL: origin + updateManifestAsset})
					}
					if scenario == "foreign-asset" {
						assets[0].URL = strings.Replace(origin, "/respire/", "/respire-client2/", 1) + name
					}
					bytes, _ := json.Marshal([]any{map[string]any{
						"tag_name": "client2-v2.0.0", "published_at": "2026-11-25T00:00:00Z",
						"draft": false, "prerelease": false, "assets": assets,
					}})
					return reply(200, bytes)
				case origin + updateManifestAsset:
					if scenario == "invalid-manifest" {
						return reply(200, []byte("null"))
					}
					release := manifestRelease{Version: "2.0.0", Artifacts: []manifestArtifact{{
						Filename: name, URL: "https://github.com/risense-ai/respire-client2/releases/download/v2.0.0/" + name,
						Size: int64(len(binary)), SHA256: base64.StdEncoding.EncodeToString(digest[:]),
					}}}
					encoded, _ := json.Marshal(updateManifest{SchemaVersion: 1, Stable: release})
					return reply(200, encoded)
				case origin + name:
					if scenario == "hash-error" {
						return reply(200, bytes.Repeat([]byte("x"), len(binary)))
					}
					if scenario == "partial" {
						return reply(200, binary[:len(binary)-1])
					}
					return reply(200, binary)
				default:
					t.Fatalf("unexpected fallback request: %s", req.URL)
					return nil, nil
				}
			})}
			p := newReleaseProvider(client, func() host.Preferences { return host.Preferences{ClientChannel: "stable"} }, nil, nil)
			release, err := p.Check(context.Background(), updater.CheckRequest{CurrentVersion: "1.0.0", Platform: "windows", Arch: "amd64"})
			switch scenario {
			case "valid", "hash-error", "partial":
				if err != nil || release == nil {
					t.Fatal("expected bound update", err)
				}
				var out bytes.Buffer
				err = p.Download(context.Background(), release, &out, func(int64, int64) {})
				if scenario == "valid" && (err != nil || !bytes.Equal(out.Bytes(), binary)) {
					t.Fatal("exact bytes not accepted", err)
				}
				if scenario != "valid" && err == nil {
					t.Fatal("corrupted or partial downloads cannot complete")
				}
			default:
				if err == nil || release != nil {
					t.Fatal("invalid primary unexpectedly accepted")
				}
			}
		})
	}
}
