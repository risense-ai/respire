//go:build linux || windows || (darwin && cgo)

package host

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"os"
	"path/filepath"
	"testing"
)

func TestRealRuntimeDesktopColdKeygen(t *testing.T) {
	m, info := realRuntimeManager(t)
	m.ClientVersion = "0.4.0"
	ctx := realRuntimeContext(t)
	op, err := controlUUID()
	if err != nil {
		t.Fatal(err)
	}
	id := "desktop-" + op
	backend, slot := runtimeLoginBackend(), "super:"+offlineKeygenAlias(op)
	var factor string
	stores, writes := 0, 0
	openStore := func(context.Context) hostCredentialStore {
		stores++
		return hostCredentialStore{
			backends: []string{backend},
			read: func(selected, service, account string) (*string, error) {
				if selected != backend || service != "rsrs" || account != slot {
					return nil, errors.New("unexpected synthetic credential lookup")
				}
				if factor == "" {
					return nil, nil
				}
				copy := factor
				return &copy, nil
			},
			preserve: func(service, account, value string) error {
				if service != "rsrs" || account != slot || value == "" || factor != "" {
					return errors.New("unexpected or repeated synthetic credential write")
				}
				factor = value
				writes++
				return nil
			},
		}
	}
	binding, err := m.selectedColdKeygenBinding(info, op)
	if err != nil {
		t.Fatal(err)
	}
	raw, shown, err := m.desktopOfflineKeygenWithStore(ctx, id, false, openStore)
	if err != nil || writes != 1 || stores != 1 || shown == "" || shown != factor || bytes.Contains(raw, []byte(factor)) {
		t.Fatalf("fresh desktop cold keygen failed: %v", err)
	}
	var receipt struct {
		Summary struct {
			Operation string `json:"operation_id"`
			Outcome   string `json:"outcome"`
			Digest    string `json:"session_sha256"`
		} `json:"summary"`
	}
	session := filepath.Join(binding.Profile, "session.json")
	original, err := os.ReadFile(session)
	if err != nil || json.Unmarshal(raw, &receipt) != nil || receipt.Summary.Operation != op ||
		receipt.Summary.Outcome != "committed" || receipt.Summary.Digest != controlDigest(original) {
		t.Fatal("receipt did not identify exact original session bytes")
	}
	if recovered, revealed, err := m.desktopOfflineKeygenWithStore(ctx, id, true, openStore); err != nil ||
		revealed != "" || !bytes.Equal(raw, recovered) || writes != 1 || stores != 2 {
		t.Fatalf("original desktop result was not recovered without factor: %v", err)
	}
	if _, _, err := m.desktopOfflineKeygenWithStore(ctx, id, false, openStore); err == nil || writes != 1 || stores != 2 {
		t.Fatal("existing custody allowed a second send")
	}

	image, err := os.OpenFile(binding.Image, os.O_WRONLY|os.O_APPEND, 0)
	if err != nil {
		t.Fatal(err)
	}
	_, err = image.Write([]byte("synthetic changed selected image"))
	if err := errors.Join(err, image.Close()); err != nil {
		t.Fatal(err)
	}
	if _, _, err := m.desktopOfflineKeygenWithStore(ctx, id, true, openStore); err == nil || stores != 2 {
		t.Fatal("altered selected image reached credential handling")
	}
	if after, err := os.ReadFile(session); err != nil || !bytes.Equal(after, original) {
		t.Fatal("recovery or selection refusal changed session bytes")
	}
	for _, name := range []string{"offline-keygen.json", "offline-keygen-result-" + op + ".json", "maintenance.json", "maintenance.keygen-" + op + ".json"} {
		if _, err := os.Lstat(filepath.Join(m.DataRoot, "runtime", "autostart", name)); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("unexpected legacy shared file %s: %v", name, err)
		}
	}
	if _, _, err := m.Health(ctx); !IsUnavailable(err) {
		t.Fatalf("fresh empty-profile fixture unexpectedly has a serving Runtime: %v", err)
	}
}
