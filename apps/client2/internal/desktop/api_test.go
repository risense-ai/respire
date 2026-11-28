package desktop

import (
	"encoding/json"
	"reflect"
	"strings"
	"testing"

	"respire-client2/internal/host"
)

func TestPreferencesContractReflection(t *testing.T) {
	intentionallyNonPatchable := map[string]string{}
	var prefs host.Preferences
	prefType := reflect.TypeOf(prefs)
	fields := make(map[string]reflect.StructField)
	for _, field := range reflect.VisibleFields(prefType) {
		key, _, _ := strings.Cut(field.Tag.Get("json"), ",")
		if field.IsExported() && key != "" && key != "-" {
			fields[key] = field
		}
	}
	for key := range intentionallyNonPatchable {
		if _, ok := fields[key]; !ok {
			t.Errorf("non-patchable key %q has no Preferences field", key)
		}
	}
	inputs := make(map[string]reflect.Kind)
	for _, field := range reflect.VisibleFields(reflect.TypeOf(actionInput{})) {
		key, _, _ := strings.Cut(field.Tag.Get("json"), ",")
		if field.IsExported() {
			inputs[key] = field.Type.Kind()
		}
	}
	keys := make(map[string]bool)
	targets := preferencePatchTargets(&prefs)
	for _, key := range preferencePatchKeys {
		if _, ok := fields[key]; !ok || keys[key] {
			t.Errorf("unbacked or duplicate allowlist key %q", key)
		}
		keys[key] = true
	}
	for key := range targets {
		if _, ok := fields[key]; !ok {
			t.Errorf("unbacked patch target %q", key)
		}
	}
	for key, field := range fields {
		if _, excluded := intentionallyNonPatchable[key]; excluded {
			if _, hasTarget := targets[key]; keys[key] || hasTarget {
				t.Errorf("excluded field %q is patchable", key)
			}
			continue
		}
		if !keys[key] {
			t.Errorf("missing allowlist key %q", key)
		}
		wantTarget := reflect.ValueOf(&prefs).Elem().FieldByIndex(field.Index).Addr().Interface()
		if targets[key] != wantTarget {
			t.Errorf("patch target %q must point to Preferences.%s", key, field.Name)
		}
		kind := field.Type.Kind()
		if (kind != reflect.Bool && kind != reflect.Int && kind != reflect.String) || inputs[key] != kind {
			t.Errorf("actionInput field %q has kind %v, want %v", key, inputs[key], kind)
		}
	}
}

// The POST /api/* shape check must carry one JSON field per patchable
// preference key: the completion step of the onboarding wizard posts only
// onboarding keys, and a shape check without them rejects a valid patch
// with `json: unknown field "onboardingCompleted"`.
func TestDecodeActionBodyAcceptsEveryPreferenceKey(t *testing.T) {
	var prefs host.Preferences
	targets := preferencePatchTargets(&prefs)
	body := map[string]any{}
	for _, key := range preferencePatchKeys {
		switch targets[key].(type) {
		case *bool:
			body[key] = true
		case *int:
			body[key] = 1
		case *string:
			body[key] = "x"
		default:
			t.Fatalf("preference key %s has unsupported target type %T", key, targets[key])
		}
	}
	data, err := json.Marshal(body)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := decodeActionBody(data); err != nil {
		t.Fatalf("shape check rejected the full preference patch: %v", err)
	}
}

func TestDecodeActionBodyAcceptsWizardCompletionPatch(t *testing.T) {
	input, err := decodeActionBody([]byte(`{"onboardingCompleted":true,"onboardingStep":"done"}`))
	if err != nil {
		t.Fatalf("wizard completion patch must pass the shape check: %v", err)
	}
	if !input.OnboardingCompleted || input.OnboardingStep != "done" {
		t.Fatalf("decoded fields mismatch, got %+v", input)
	}
}

func TestDecodeActionBodyStillRejectsUnknownAndTrailing(t *testing.T) {
	if _, err := decodeActionBody([]byte(`{"modelMirror":42}`)); err == nil {
		t.Fatal("invalid field types must stay rejected")
	}
	if _, err := decodeActionBody([]byte(`{"noSuchField":1}`)); err == nil {
		t.Fatal("unknown fields must stay rejected")
	}
	if _, err := decodeActionBody([]byte(`{"theme":"dark"} {"theme":"light"}`)); err == nil {
		t.Fatal("trailing JSON objects must stay rejected")
	}
}
