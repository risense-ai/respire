package host

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"net/http"
	"regexp"
	"strconv"
	"strings"

	"golang.org/x/mod/semver"
)

const monoReleaseRepo = "respire"

var componentVersion = regexp.MustCompile(`^(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-dev\.(0|[1-9][0-9]*))?$`)

// PrimaryComponentRelease only reports absence after successfully reading a
// complete official catalog. A chosen but incomplete release is not absence.
func PrimaryComponentRelease(ctx context.Context, client *http.Client, component, channel string) (*ReleaseInfo, error) {
	switch component {
	case "runtime", "cli", "client2":
	default:
		return nil, errors.New("invalid release component")
	}
	if channel != "stable" && channel != "dev" {
		return nil, errors.New("invalid release channel")
	}
	prefix := component + "-v"
	type catalogEntry struct {
		ReleaseInfo
		PublishedAt  string `json:"published_at"`
		IsDraft      *bool  `json:"draft"`
		IsPrerelease *bool  `json:"prerelease"`
	}
	var chosen *ReleaseInfo
	seen := make(map[string]bool)
	for page := 1; page <= 40; page++ {
		url := fmt.Sprintf("https://api.github.com/repos/risense-ai/respire/releases?per_page=100&page=%d", page)
		var entries []catalogEntry
		if err := fetchOfficialComponentCatalog(ctx, client, url, &entries); err != nil {
			return nil, err
		}
		if entries == nil || len(entries) > 100 {
			return nil, errors.New("invalid component release catalog")
		}
		for _, entry := range entries {
			if entry.Tag == "" || entry.IsDraft == nil || entry.IsPrerelease == nil {
				return nil, errors.New("incomplete release catalog entry")
			}
			entry.Draft, entry.Prerelease = *entry.IsDraft, *entry.IsPrerelease
			version, applies := strings.CutPrefix(entry.Tag, prefix)
			if !applies || entry.Draft {
				continue
			}
			match := componentVersion.FindStringSubmatch(version)
			if len(match) != 5 || match[0] != version || entry.Prerelease != (match[4] != "") ||
				entry.PublishedAt == "" || seen[entry.Tag] {
				return nil, errors.New("invalid or duplicate published component release")
			}
			for _, number := range match[1:] {
				if number == "" {
					continue
				}
				n, err := strconv.ParseUint(number, 10, 64)
				if err != nil || n > 9_007_199_254_740_991 {
					return nil, errors.New("invalid component release version number")
				}
			}
			seen[entry.Tag] = true
			if channel == "stable" && entry.Prerelease {
				continue
			}
			if chosen == nil || semver.Compare("v"+version, chosen.MetadataTag()) > 0 {
				selected := entry.ReleaseInfo
				selected.component = component
				chosen = &selected
			}
		}
		if len(entries) < 100 {
			return chosen, nil
		}
	}
	return nil, errors.New("component release catalog exceeds page limit")
}

func fetchOfficialComponentCatalog(ctx context.Context, client *http.Client, url string, out any) error {
	official := *client
	official.CheckRedirect = func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return err
	}
	req.Header.Set("Accept", "application/vnd.github+json")
	req.Header.Set("User-Agent", "Respire-Client2")
	response, err := official.Do(req)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return &httpStatusError{code: response.StatusCode}
	}
	body, err := io.ReadAll(io.LimitReader(response.Body, 2<<20+1))
	if err != nil {
		return err
	}
	if len(body) > 2<<20 {
		return errors.New("component release catalog exceeds response limit")
	}
	return json.Unmarshal(body, out)
}

// MetadataTag is the legacy-neutral wire tag retained in byte-identical mirrors.
func (r ReleaseInfo) MetadataTag() string {
	if r.component != "" {
		return strings.TrimPrefix(r.Tag, r.component+"-")
	}
	return r.Tag
}

func (r ReleaseInfo) assetRepository(legacy string) (string, error) {
	if r.component == "" {
		return legacy, nil
	}
	if legacy != "respire-"+r.component {
		return "", errors.New("release does not belong to requested component")
	}
	return monoReleaseRepo, nil
}

func (m *Manager) latestComponentRelease(ctx context.Context, component, repo, channel string) (ReleaseInfo, error) {
	primary, err := PrimaryComponentRelease(ctx, m.HTTPClient, component, channel)
	if err != nil {
		return ReleaseInfo{}, err
	}
	if primary != nil {
		return *primary, nil
	}
	list, err := m.fetchReleaseList(ctx, repo, channel)
	if err != nil {
		return ReleaseInfo{}, err
	}
	if channel == "stable" && (len(list) != 1 || list[0].Prerelease || semver.Prerelease(list[0].Tag) != "") {
		return ReleaseInfo{}, errors.New("stable channel returned invalid release")
	}
	return SelectRelease(list, channel)
}

func (m *Manager) componentMetadata(ctx context.Context, release ReleaseInfo, url string, raw *json.RawMessage) error {
	if release.component != "" {
		return m.fetchJSONOnce(ctx, url, raw)
	}
	return m.fetchJSON(ctx, url, raw)
}
