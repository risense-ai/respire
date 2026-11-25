package desktop

import "respire-client2/internal/i18n"

import (
	"context"
	"crypto/sha256"
	"crypto/subtle"
	"encoding/base64"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strings"

	"github.com/wailsapp/wails/v3/pkg/updater"
	"golang.org/x/mod/semver"
	"respire-client2/internal/host"
)

const updateManifestAsset = "client-updates.json"

type manifestArtifact struct {
	Filename string `json:"filename"`
	URL      string `json:"url"`
	Size     int64  `json:"size"`
	SHA256   string `json:"sha256"`
}

type manifestRelease struct {
	Version     string             `json:"version"`
	Notes       string             `json:"notes"`
	PublishedAt string             `json:"publishedAt"`
	Artifacts   []manifestArtifact `json:"artifacts"`
}

type updateManifest struct {
	SchemaVersion int             `json:"schemaVersion"`
	Stable        manifestRelease `json:"stable"`
	Dev           manifestRelease `json:"dev"`
}

type releaseProvider struct {
	client      *http.Client
	preferences func() host.Preferences
	stable      updater.Provider
	dev         updater.Provider
}

func newReleaseProvider(client *http.Client, preferences func() host.Preferences, stable, dev updater.Provider) releaseProvider {
	return releaseProvider{client: client, preferences: preferences, stable: stable, dev: dev}
}

func (p releaseProvider) Name() string { return "respire" }

func (p releaseProvider) Check(ctx context.Context, req updater.CheckRequest) (*updater.Release, error) {
	channel := p.preferences().ClientChannel
	selected, err := host.PrimaryComponentRelease(ctx, p.client, "client2", channel)
	if err != nil {
		return nil, err
	}
	if selected == nil {
		return p.checkLegacy(ctx, req)
	}
	version := strings.TrimPrefix(selected.MetadataTag(), "v")
	if semver.Compare("v"+version, "v"+req.CurrentVersion) <= 0 {
		return nil, nil
	}
	manifestSource, err := host.ReleaseAssetURL(*selected, "respire-client2", updateManifestAsset)
	if err != nil {
		return nil, err
	}
	var document updateManifest
	if err := p.fetchJSON(ctx, manifestSource, &document); err != nil {
		return nil, err
	}
	picked := document.Stable
	if selected.Prerelease {
		picked = document.Dev
	}
	if document.SchemaVersion != 1 || picked.Version != version {
		return nil, errors.New("Client2 component update manifest identity mismatch")
	}
	filename := host.ClientAsset(req.Platform, req.Arch)
	boundURL, err := host.ReleaseAssetURL(*selected, "respire-client2", filename)
	if err != nil {
		return nil, err
	}
	var matches []manifestArtifact
	for _, artifact := range picked.Artifacts {
		if artifact.Filename == filename {
			matches = append(matches, artifact)
		}
	}
	if len(matches) != 1 || matches[0].Size <= 0 || matches[0].Size > 256<<20 {
		return nil, errors.New("Invalid component update artifact count or size")
	}
	artifact := matches[0]
	// The mirror publishes byte-identical metadata using legacy-neutral version
	// and URL fields. Verify that legacy path; never follow it for this source.
	urls, err := artifactURLs(artifact, version, filename)
	if err != nil || artifact.URL == "" || len(urls) != 2 {
		return nil, errors.New("Invalid component update artifact identity")
	}
	digest, err := base64.StdEncoding.DecodeString(artifact.SHA256)
	if err != nil || len(digest) != sha256.Size {
		return nil, errors.New("Invalid component update artifact digest")
	}
	return &updater.Release{
		Version: version, Channel: channel, Name: "Respire " + version, Notes: picked.Notes,
		Artifact:     updater.Artifact{Filename: filename, Size: artifact.Size, Platform: req.Platform, Arch: req.Arch},
		Verification: &updater.Verification{DigestAlgo: "sha256", Digest: digest},
		Metadata:     map[string]any{"respire.component-url": boundURL},
	}, nil
}

func (p releaseProvider) checkLegacy(ctx context.Context, req updater.CheckRequest) (*updater.Release, error) {
	var m updateManifest
	if err := p.fetchJSON(ctx, manifestURL(true), &m); err != nil {
		if mirrorErr := p.fetchJSON(ctx, manifestURL(false), &m); mirrorErr != nil {
			source := p.stable
			channel := "stable"
			if p.preferences().ClientChannel == "dev" {
				source, channel = p.dev, "dev"
			}
			fallback, fallbackErr := source.Check(ctx, req)
			if fallbackErr != nil {
				return nil, errors.Join(
					fmt.Errorf(i18n.Message077398250a19534b, mirrorErr),
					fmt.Errorf(i18n.Message37076d81e9d4552d, err),
					fmt.Errorf(i18n.Message348fba8de8416346, fallbackErr),
				)
			}
			if fallback == nil {
				return nil, nil
			}
			if fallback.Verification == nil || fallback.Verification.DigestAlgo != "sha256" || len(fallback.Verification.Digest) != 32 {
				return nil, errors.New(i18n.Messagead047278f7057599)
			}
			fallback.Channel = channel
			fallback.Metadata["respire.fallback"] = channel
			return fallback, nil
		}
	}
	if m.SchemaVersion != 1 {
		return nil, fmt.Errorf(i18n.Message1f40bbfea0578c9c, m.SchemaVersion)
	}
	candidates := []manifestRelease{m.Stable}
	if p.preferences().ClientChannel == "dev" {
		candidates = append(candidates, m.Dev)
	}
	current := "v" + req.CurrentVersion
	var picked *manifestRelease
	for i := range candidates {
		item := &candidates[i]
		if !semver.IsValid("v"+item.Version) || (picked != nil && semver.Compare("v"+picked.Version, "v"+item.Version) >= 0) {
			continue
		}
		if p.preferences().ClientChannel != "dev" && semver.Prerelease("v"+item.Version) != "" {
			continue
		}
		if semver.Compare("v"+item.Version, current) <= 0 {
			continue
		}
		picked = item
	}
	if picked == nil {
		return nil, nil
	}
	filename := host.ClientAsset(req.Platform, req.Arch)
	var artifact *manifestArtifact
	for i := range picked.Artifacts {
		if picked.Artifacts[i].Filename == filename {
			artifact = &picked.Artifacts[i]
			break
		}
	}
	if artifact == nil {
		return nil, fmt.Errorf(i18n.Message57d3274f6f4513dc, host.Platform())
	}
	urls, err := artifactURLs(*artifact, picked.Version, filename)
	if err != nil {
		return nil, err
	}
	digest, err := base64.StdEncoding.DecodeString(artifact.SHA256)
	if err != nil || len(digest) != 32 {
		return nil, errors.New(i18n.Messageaa8bd38d3cb81780)
	}
	return &updater.Release{
		Version: picked.Version, Channel: p.preferences().ClientChannel, Name: "Respire " + picked.Version,
		Notes: picked.Notes, Artifact: updater.Artifact{Filename: filename, Size: artifact.Size, Platform: req.Platform, Arch: req.Arch},
		Verification: &updater.Verification{DigestAlgo: "sha256", Digest: digest},
		Metadata:     map[string]any{"respire.urls": urls},
	}, nil
}

func (p releaseProvider) Download(ctx context.Context, r *updater.Release, dst io.Writer, progress func(int64, int64)) error {
	if stored, exists := r.Metadata["respire.component-url"]; exists {
		source, ok := stored.(string)
		if !ok || r.Artifact.Filename != host.ClientAsset(r.Artifact.Platform, r.Artifact.Arch) ||
			r.Verification == nil || r.Verification.DigestAlgo != "sha256" || len(r.Verification.Digest) != sha256.Size ||
			r.Artifact.Size <= 0 || r.Artifact.Size > 256<<20 ||
			source != "https://github.com/risense-ai/respire/releases/download/client2-v"+r.Version+"/"+r.Artifact.Filename {
			return errors.New("Invalid bound Client2 component download")
		}
		counter := &componentDownloadWriter{remaining: r.Artifact.Size}
		hash := sha256.New()
		counter.dest = io.MultiWriter(dst, hash)
		if err := p.download(ctx, source, r.Artifact.Size, counter, progress); err != nil {
			return err
		}
		if counter.remaining != 0 || subtle.ConstantTimeCompare(hash.Sum(nil), r.Verification.Digest) != 1 {
			return errors.New("Client2 component download size or digest mismatch")
		}
		return nil
	}
	if channel, ok := r.Metadata["respire.fallback"].(string); ok {
		source := p.stable
		if channel == "dev" {
			source = p.dev
		}
		return source.Download(ctx, r, dst, progress)
	}
	raw, ok := r.Metadata["respire.urls"].([]string)
	if !ok || len(raw) == 0 {
		return errors.New(i18n.Message6619761c7ac9d084)
	}
	var failures []error
	for _, source := range raw {
		limit := r.Artifact.Size
		if limit == 0 {
			limit = 256 << 20
		}
		err := p.download(ctx, source, limit, dst, progress)
		if err == nil {
			return nil
		}
		failures = append(failures, fmt.Errorf("%s: %w", sourceLabel(source), err))
	}
	return errors.Join(failures...)
}

type componentDownloadWriter struct {
	dest      io.Writer
	remaining int64
}

func (w *componentDownloadWriter) Write(bytes []byte) (int, error) {
	if int64(len(bytes)) > w.remaining {
		return 0, errors.New("Client2 component download exceeds expected size")
	}
	n, err := w.dest.Write(bytes)
	w.remaining -= int64(n)
	return n, err
}
func (p releaseProvider) download(ctx context.Context, source string, limit int64, dst io.Writer, progress func(int64, int64)) error {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, source, nil)
	if err != nil {
		return err
	}
	request.Header.Set("Accept", "application/octet-stream")
	response, err := p.client.Do(request)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return fmt.Errorf("HTTP %d", response.StatusCode)
	}
	if response.ContentLength > limit {
		return errors.New(i18n.Message7e4b5fa10d50dc4d)
	}
	reader := io.LimitReader(response.Body, limit+1)
	buffer := make([]byte, 128<<10)
	var written int64
	for {
		n, readErr := reader.Read(buffer)
		if n > 0 {
			if _, writeErr := dst.Write(buffer[:n]); writeErr != nil {
				return writeErr
			}
			written += int64(n)
			progress(written, response.ContentLength)
		}
		if readErr != nil {
			if readErr == io.EOF {
				return nil
			}
			return readErr
		}
	}
}

func (p releaseProvider) fetchJSON(ctx context.Context, source string, value any) error {
	request, err := http.NewRequestWithContext(ctx, http.MethodGet, source, nil)
	if err != nil {
		return err
	}
	request.Header.Set("Accept", "application/json")
	response, err := p.client.Do(request)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return fmt.Errorf("HTTP %d", response.StatusCode)
	}
	data, err := io.ReadAll(io.LimitReader(response.Body, 2<<20+1))
	if err != nil {
		return err
	}
	if len(data) > 2<<20 {
		return errors.New(i18n.Message729471fced21b0cc)
	}
	if err = json.Unmarshal(data, value); err != nil {
		return fmt.Errorf(i18n.Message27788a0b0f509925, err)
	}
	return nil
}

func manifestURL(mirror bool) string {
	source := "https://github.com/risense-ai/respire-client2/releases/latest/download/" + updateManifestAsset
	if mirror {
		return "https://gh-proxy.com/" + source
	}
	return source
}

func artifactURLs(item manifestArtifact, releaseVersion, filename string) ([]string, error) {
	if item.Filename != filename || item.SHA256 == "" || item.Size < 0 {
		return nil, errors.New(i18n.Message199157d950af577d)
	}
	base := "https://github.com/risense-ai/respire-client2/releases/download/v" + releaseVersion + "/" + filename
	direct, err := url.Parse(base)
	if err != nil {
		return nil, err
	}
	if item.URL != "" {
		parsed, err := url.Parse(item.URL)
		if err != nil || parsed.Scheme != "https" || parsed.Host != direct.Host || parsed.Path != direct.Path {
			return nil, errors.New(i18n.Message5ce7153f319d5aea)
		}
	}
	if item.URL != "" && item.URL != base {
		return nil, errors.New(i18n.Message71e1860911aeece1)
	}
	return []string{"https://gh-proxy.com/" + base, base}, nil
}

func sourceLabel(source string) string {
	if strings.Contains(source, "gh-proxy.com") {
		return i18n.Messagee18e38d45cde0d67
	}
	return "GitHub"
}
