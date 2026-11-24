package host

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

type pausedModelIngress struct {
	source io.Reader
	ctx    context.Context
}

func (r pausedModelIngress) Read(p []byte) (int, error) {
	n, err := r.source.Read(p)
	if n != 0 || !errors.Is(err, io.EOF) {
		return n, err
	}
	<-r.ctx.Done()
	return 0, r.ctx.Err()
}
func verifyModelIngressRemainsReadableAndCancels(t *testing.T, ctx context.Context, m *Manager, ready HealthInfo, source string) {
	t.Helper()
	const title = "Synthetic model ingress witness"
	const content = "Scoped read remains available during synthetic model ingress."
	path := filepath.Join(m.DataRoot, "synthetic-model-ingress-witness.json")
	seed, err := json.Marshal([]map[string]string{{"title": title, "content": content, "kind": "context", "importance": "normal", "user": "client2-disposable-runtime"}})
	if err != nil {
		t.Fatal(err)
	}
	file, err := os.OpenFile(path, os.O_CREATE|os.O_EXCL|os.O_WRONLY, 0600)
	if err != nil {
		t.Fatal(err)
	}
	_, err = file.Write(seed)
	if err = errors.Join(err, file.Close()); err != nil {
		t.Fatal(err)
	}
	defer os.Remove(path)
	object := func(value any) map[string]any { v, _ := value.(map[string]any); return v }
	command := func(c context.Context, verb string, args ...string) map[string]any {
		t.Helper()
		id := fmt.Sprintf("synthetic-ingress-%s-%d", verb, time.Now().UnixNano())
		raw, err := m.legacyRPCRequest(c, map[string]any{"v": 1, "id": id, "method": "cli.exec", "args": append([]string{"--json", verb}, args...), "scope": runtimeScope{ready.AccountID, ready.InstanceID, ready.Generation}, "operation_id": id, "idempotency_key": id})
		if err != nil {
			t.Fatalf("synthetic scoped %s transport: %v", verb, err)
		}
		var reply map[string]any
		if err := json.Unmarshal(raw, &reply); err != nil {
			t.Fatal(err)
		}
		scope := object(reply["scope"])
		envelope := object(reply["envelope"])
		if reply["v"] != float64(1) || reply["id"] != id || reply["operation_id"] != id || reply["idempotency_key"] != id || reply["pid"] != float64(ready.PID) ||
			scope["account_id"] != ready.AccountID || scope["instance_id"] != ready.InstanceID || scope["generation"] != float64(ready.Generation) || reply["ok"] != true || reply["exit"] != float64(0) || envelope["command"] != verb || envelope["status"] != "ok" {
			t.Fatalf("synthetic scoped %s lacked matching success receipt; code=%v", verb, reply["code"])
		}
		return envelope
	}
	imported := object(command(ctx, "import", path)["summary"])
	if imported["imported"] != float64(1) || imported["duplicates"] != float64(0) {
		t.Fatal("real import did not seed exactly one synthetic witness")
	}
	rows, ok := command(ctx, "list", "--limit", "10")["details"].([]any)
	if !ok {
		t.Fatal("real list did not return entries")
	}
	var entryID string
	for _, row := range rows {
		record := object(row)
		if inner := object(record["entry"]); inner != nil {
			record = inner
		}
		if record["title"] == title {
			id, _ := record["id"].(string)
			if entryID != "" || !runtimeInstanceID.MatchString(id) {
				t.Fatal("synthetic witness identity invalid or ambiguous")
			}
			entryID = id
		}
	}
	if entryID == "" {
		t.Fatal("synthetic witness missing")
	}
	checkShow := func() {
		t.Helper()
		c, cancel := context.WithTimeout(ctx, 5*time.Second)
		defer cancel()
		row := object(object(command(c, "show", entryID)["details"])["entry"])
		if row["id"] != entryID || row["content"] != content {
			t.Fatal("compiled legacy show did not return exact decrypted synthetic data")
		}
		got, _, err := m.Health(c)
		if err != nil || !got.StoreReady || got.PID != ready.PID || got.InstanceID != ready.InstanceID || got.AccountID != ready.AccountID || got.Generation != ready.Generation {
			t.Fatalf("ingress changed serving Runtime: %v", err)
		}
	}
	checkShow()
	profile, err := canonicalSessionDirectory(ready.DataDir)
	if err != nil {
		t.Fatal(err)
	}
	namespace := filepath.Join(m.DataRoot, "runtime", "model-assets-v1", controlDigest([]byte(profile)))
	dest := filepath.Join(namespace, "m3")
	checkAbsent := func() {
		t.Helper()
		if _, err := os.Lstat(dest); !errors.Is(err, os.ErrNotExist) {
			t.Fatalf("unsealed M3 was visible: %v", err)
		}
	}
	checkAbsent()
	weights, err := os.Open(filepath.Join(source, "model_quantized.onnx"))
	if err != nil {
		t.Fatal(err)
	}
	defer weights.Close()
	tokenizer, err := os.Open(filepath.Join(source, "tokenizer.json"))
	if err != nil {
		t.Fatal(err)
	}
	defer tokenizer.Close()
	outer, cancel := context.WithCancel(ctx)
	done := make(chan struct{})
	var uploadErr error
	go func() {
		defer close(done)
		uploadErr = m.installRuntimeOwnedModel(outer, ready, "m3", pausedModelIngress{io.LimitReader(weights, 1<<20), outer}, tokenizer)
	}()
	defer func() {
		cancel()
		select {
		case <-done:
		case <-time.After(5 * time.Second):
			t.Error("owned synthetic uploader did not exit")
		}
	}()

	stageBytes := func() (anyStage, haveBytes bool, err error) {
		entries, err := os.ReadDir(namespace)
		if errors.Is(err, os.ErrNotExist) {
			return false, false, nil
		}
		if err != nil {
			return false, false, err
		}
		for _, d := range entries {
			if !strings.HasPrefix(d.Name(), ".incoming-") {
				continue
			}
			anyStage = true
			f, err := os.Stat(filepath.Join(namespace, d.Name(), "model_quantized.onnx"))
			if err != nil && !errors.Is(err, os.ErrNotExist) {
				return false, false, err
			}
			haveBytes = haveBytes || err == nil && f.Size() > 0
		}
		return anyStage, haveBytes, nil
	}
	tick := time.NewTicker(20 * time.Millisecond)
	defer tick.Stop()
	wait := time.NewTimer(12 * time.Second)
	defer wait.Stop()
	for {
		_, received, err := stageBytes()
		if err != nil {
			t.Fatal(err)
		}
		if received {
			break
		}
		select {
		case <-done:
			t.Fatalf("upload ended before Runtime received staged bytes: %v", uploadErr)
		case <-ctx.Done():
			t.Fatal(ctx.Err())
		case <-wait.C:
			t.Fatal("Runtime never wrote ingress bytes")
		case <-tick.C:
		}
	}
	checkAbsent()
	checkShow()
	cancel()
	select {
	case <-done:
		if !errors.Is(uploadErr, errOnlineModelUnconfirmed) || !errors.Is(uploadErr, context.Canceled) {
			t.Fatalf("cancelled upload should stay unconfirmed: %v", uploadErr)
		}
	case <-time.After(10 * time.Second):
		t.Fatal("owned synthetic uploader did not finish")
	}
	cleanup := time.NewTimer(10 * time.Second)
	defer cleanup.Stop()
	for {
		anyStage, _, err := stageBytes()
		if err != nil {
			t.Fatal(err)
		}
		if !anyStage {
			break
		}
		select {
		case <-ctx.Done():
			t.Fatal(ctx.Err())
		case <-cleanup.C:
			t.Fatal("cancelled Runtime ingress left staging")
		case <-tick.C:
		}
	}
	checkAbsent()
	checkShow()
	t.Log("verified actual Runtime ingress bytes, concurrent scoped legacy show, cancellation and staging cleanup")
}
