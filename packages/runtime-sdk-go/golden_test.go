package runtimesdk

import (
	"encoding/json"
	"os"
	"path/filepath"
	"reflect"
	"testing"
)

func golden(t *testing.T, file string, dto any) {
	t.Helper()
	data, err := os.ReadFile(filepath.Join("..", "..", "contracts", "runtime-api", "fixtures", file))
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(data, dto); err != nil {
		t.Fatal(err)
	}
	encoded, err := json.Marshal(dto)
	if err != nil {
		t.Fatal(err)
	}
	var before, after any
	if err := json.Unmarshal(data, &before); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(encoded, &after); err != nil {
		t.Fatal(err)
	}
	if !reflect.DeepEqual(before, after) {
		t.Fatalf("golden JSON changed after typed round trip:\ninput:  %s\noutput: %s", data, encoded)
	}
}

func TestGoldenRoundTrips(t *testing.T) {
	t.Run("problem", func(t *testing.T) { golden(t, "problem-details.json", &ProblemDetails{}) })
	t.Run("operation", func(t *testing.T) { golden(t, "operation-event.json", &OperationEvent{}) })
	t.Run("memory", func(t *testing.T) { golden(t, "memory-item.json", &MemoryItem{}) })
	t.Run("catalog", func(t *testing.T) { golden(t, "model-catalog-response.json", &ModelCatalogResponse{}) })
}
