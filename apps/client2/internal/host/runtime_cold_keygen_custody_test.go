//go:build linux || windows || (darwin && cgo)

package host

import (
	"bytes"
	"os"
	"path/filepath"
	"strings"
	"sync"
	"testing"
)

func newColdKeygenCustodyFixture(t *testing.T) (*Manager, coldKeygenCustodyBinding, runtimeRegisterMaterial) {
	t.Helper()
	root := t.TempDir()
	m := &Manager{DataRoot: root, Root: filepath.Join(root, "client2")}
	if err := os.Mkdir(m.Root, 0700); err != nil {
		t.Fatal(err)
	}
	b := coldKeygenCustodyBinding{
		Operation: "4576ffb6-d245-4b73-86cf-59866145cb2a", Profile: root,
		Image: filepath.Join(root, "synthetic-executable"), ImageSHA: strings.Repeat("a", 64),
	}
	material := runtimeRegisterMaterial{
		Account: offlineKeygenAlias(b.Operation), Backend: runtimeLoginBackend(), Salt: strings.Repeat("b", 32),
		Wrapped: "rsrs:v1:" + strings.Repeat("c", 96), Nonce: strings.Repeat("d", 24), Super: "never-persist-this-test-factor",
	}
	return m, b, material
}

func TestColdKeygenCustodyPrepareAndSingleSend(t *testing.T) {
	m, binding, material := newColdKeygenCustodyFixture(t)
	intent, err := m.prepareColdKeygenCustody(binding, material)
	if err != nil {
		t.Fatal(err)
	}
	path, sent, err := m.coldKeygenCustodyPaths(binding, false)
	if err != nil {
		t.Fatal(err)
	}
	raw, err := os.ReadFile(path)
	if err != nil || bytes.Contains(raw, []byte(material.Super)) || bytes.Contains(raw, []byte("super_password")) {
		t.Fatal("unexpected preparation content")
	}
	if _, err := m.prepareColdKeygenCustody(binding, material); err == nil {
		t.Fatal("duplicate prepare replaced existing custody")
	}
	if loaded, err := m.loadColdKeygenCustody(binding); err != nil || loaded != intent {
		t.Fatalf("failed to read prepared stage: %v", err)
	}

	const racers = 8
	var race sync.WaitGroup
	outcomes := make(chan error, racers)
	for range racers {
		race.Go(func() { outcomes <- m.checkpointColdKeygenSend(intent) })
	}
	race.Wait()
	close(outcomes)
	winners := 0
	for err := range outcomes {
		if err == nil {
			winners++
		}
	}
	if winners != 1 {
		t.Fatalf("expected one dispatch permit; got %d", winners)
	}
	intent.Sent = true
	if loaded, err := m.loadColdKeygenCustody(binding); err != nil || loaded != intent {
		t.Fatalf("failed to read sent stage: %v", err)
	}
	if err := m.checkpointColdKeygenSend(intent); err == nil {
		t.Fatal("returned a dispatch permit for a sent operation")
	}
	marker, err := os.ReadFile(sent)
	if err != nil || bytes.Contains(marker, []byte(material.Super)) {
		t.Fatal("invalid marker for sent operation")
	}
	after, err := os.ReadFile(path)
	if err != nil || !bytes.Equal(raw, after) {
		t.Fatal("immutable preparation changed")
	}
}

func TestColdKeygenCustodyRejectsChangedBindingAndBytes(t *testing.T) {
	m, binding, material := newColdKeygenCustodyFixture(t)
	intent, err := m.prepareColdKeygenCustody(binding, material)
	if err != nil {
		t.Fatal(err)
	}
	for _, attempted := range []coldKeygenCustodyBinding{
		{binding.Operation, filepath.Join(binding.Profile, "other"), binding.Image, binding.ImageSHA},
		{binding.Operation, binding.Profile, binding.Image, strings.Repeat("b", 64)},
		{"../arbitrary", binding.Profile, binding.Image, binding.ImageSHA},
	} {
		if _, err := m.loadColdKeygenCustody(attempted); err == nil {
			t.Fatal("accepted an altered binding")
		}
	}
	mutated := intent
	mutated.Intent.Material.Nonce = strings.Repeat("e", 24)
	if err := m.checkpointColdKeygenSend(mutated); err == nil {
		t.Fatal("accepted altered prepared intent")
	}
	path, sent, err := m.coldKeygenCustodyPaths(binding, false)
	if err != nil {
		t.Fatal(err)
	}
	if err := m.checkpointColdKeygenSend(intent); err != nil {
		t.Fatal(err)
	}
	if err := os.WriteFile(sent, []byte("{}"), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := m.loadColdKeygenCustody(binding); err == nil {
		t.Fatal("accepted an altered send marker")
	}
	if err := os.WriteFile(path, []byte(strings.Repeat("x", coldKeygenCustodyLimit+1)), 0600); err != nil {
		t.Fatal(err)
	}
	if _, err := m.loadColdKeygenCustody(binding); err == nil {
		t.Fatal("accepted an oversized preparation record")
	}
}
