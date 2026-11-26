package host

import (
	"context"
	"encoding/json"
	"errors"
	"net"
	"net/http"
	"slices"
	"sync/atomic"
	"time"

	runtimesdk "github.com/risense-ai/respire/packages/runtime-sdk-go"
)

// DashboardHealth discovers the Runtime instance before reading the legacy
// process and account fields still needed by the desktop dashboard.
func (m *Manager) DashboardHealth(ctx context.Context) (HealthInfo, json.RawMessage, error) {
	client, err := runtimesdk.NewClient(m.Endpoint, runtimesdk.WithHTTPClient(m.localClient))
	if err != nil {
		return HealthInfo{}, nil, err
	}
	discovered, err := discoverRuntime(ctx, client)
	if err != nil {
		return HealthInfo{}, nil, err
	}
	health, details, err := m.Health(ctx)
	if err != nil {
		return HealthInfo{}, nil, err
	}
	if discovered != nil && !matchesRuntime(*discovered, health) {
		return HealthInfo{}, nil, errors.New("Runtime changed during health discovery")
	}
	return health, details, nil
}

func discoverRuntime(ctx context.Context, client *runtimesdk.Client) (*runtimesdk.HealthResponse, error) {
	response, err := client.GetHealth(ctx)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	if response.StatusCode == http.StatusNotFound {
		return nil, nil
	}
	raw, err := readRuntimeJSON(response)
	if err != nil {
		return nil, err
	}
	var discovered runtimesdk.HealthResponse
	if err := json.Unmarshal(raw, &discovered); err != nil {
		return nil, err
	}
	if !discovered.Status.Valid() || discovered.InstanceId == "" || discovered.Capabilities == nil {
		return nil, errors.New("Runtime v1 health response is incomplete")
	}
	return &discovered, nil
}

func matchesRuntime(discovered runtimesdk.HealthResponse, health HealthInfo) bool {
	return health.InstanceID == discovered.InstanceId &&
		(discovered.Pid == nil || int64(health.PID) == int64(*discovered.Pid))
}

// Use the live inspection only to refuse deletion before stopping Runtime.
func (m *Manager) checkLiveM3Assets(ctx context.Context, health HealthInfo, target string) error {
	address, _, err := m.runtimeAccountEndpoint()
	if err != nil {
		return err
	}
	// A replacement listener cannot inherit the established TCP connection.
	// Even net/http's transparent retry must never redial with the bearer.
	var dialed atomic.Bool
	transport := &http.Transport{DialContext: func(ctx context.Context, network, addr string) (net.Conn, error) {
		if network != "tcp" || addr != address || !dialed.CompareAndSwap(false, true) {
			return nil, errors.New("Runtime connection changed during asset inspection")
		}
		return (&net.Dialer{}).DialContext(ctx, network, addr)
	}}
	defer transport.CloseIdleConnections()
	httpClient := &http.Client{Transport: transport, Timeout: 5 * time.Second, CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse }}
	client, err := runtimesdk.NewClient(m.Endpoint, runtimesdk.WithHTTPClient(httpClient))
	if err != nil {
		return err
	}
	discovered, err := discoverRuntime(ctx, client)
	if err != nil || discovered == nil {
		return err
	}
	if !matchesRuntime(*discovered, health) {
		return errors.New("Runtime changed during health discovery")
	}
	if !slices.Contains(discovered.Capabilities, "model.active_assets") {
		return nil
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, m.Endpoint+"/api/health", nil)
	if err != nil {
		return err
	}
	req.Header.Set("X-Respire-Health", "runtime-v1")
	res, err := httpClient.Do(req)
	if err != nil {
		return err
	}
	body, err := readRuntimeJSON(res)
	res.Body.Close()
	if err != nil {
		return err
	}
	var peer HealthInfo
	if json.Unmarshal(body, &peer) != nil || peer.InstanceID != health.InstanceID || peer.PID != health.PID || peer.ProcessStartID != health.ProcessStartID || peer.Generation != health.Generation || peer.AccountID != health.AccountID || peer.DataDir != health.DataDir {
		return errors.New("Runtime scope changed before asset inspection")
	}
	token, err := m.localRuntimeToken(false)
	if err != nil {
		return err
	}
	response, err := client.GetActiveModelAssets(ctx, func(_ context.Context, req *http.Request) error {
		req.Header.Set("Authorization", "Bearer "+token)
		return nil
	})
	if err != nil {
		return err
	}
	defer response.Body.Close()
	raw, err := readRuntimeJSON(response)
	if err != nil {
		return err
	}
	var assets runtimesdk.ActiveModelAssetsResponse
	if json.Unmarshal(raw, &assets) != nil || assets.ActivePaths == nil {
		return errors.New("Runtime active model inspection response is invalid")
	}
	if isM3Model(assets.ActiveModelId) {
		return errM3Active
	}
	inUse, err := m3AssetsUseTarget(assets.ActivePaths, target)
	if inUse {
		return errM3Active
	}
	return err
}
