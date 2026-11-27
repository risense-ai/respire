package host

import "respire-client2/internal/i18n"

import (
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
)

// parseEngineArgs reads the built argv; build() emits options before
// positional arguments, so match by prefix instead of position.
func parseEngineArgs(args []string) (engine, model string, err error) {
	for _, arg := range args {
		if value, ok := strings.CutPrefix(arg, "--model="); ok {
			model = value
		} else {
			engine = arg
		}
	}
	if engine == "" || model == "" {
		return "", "", fmt.Errorf(i18n.Messagec3e06b2a352956f5)
	}
	return engine, model, nil
}

// applyLegacyEngine saves the inference engine + weight choice in the runtime
// service and immediately reloads the model through a real inference probe.
// When the selected engine fails verification, the previous selection is
// restored and the failure is reported with its cause — never silently
// downgraded to CPU (the runtime removed cross-engine fallback in
// 1.0.20-dev.15; the client must not reintroduce it).
func (m *Manager) applyLegacyEngine(ctx context.Context, args []string, id string) (json.RawMessage, error) {
	engine, model, err := parseEngineArgs(args)
	if err != nil {
		return nil, err
	}
	scope, err := m.requestScope(ctx)
	if err != nil {
		return nil, err
	}
	if model == "quantized" {
		health, supported, err := m.runtimeModelInstallCapability(ctx)
		if err != nil {
			return nil, err
		}
		if supported {
			if _, _, managed := m.managedM3Pair(health); managed {
				known := runtimeScope{health.AccountID, health.InstanceID, health.Generation}
				if *scope != known {
					return nil, errOnlineModelUnavailable
				}
				choice, err := json.Marshal(map[string]string{
					"model": "m3", "backend": "onnx", "engine": engine, "variant": model,
				})
				if err != nil {
					return nil, err
				}
				operation := id + "/managed-switch"
				receipt, err := m.switchManagedModel(ctx, string(choice), operation, scope)
				if err != nil {
					return receipt, err
				}
				return engineEnvelope(engine, model, false, "", receipt), nil
			}
		}
	}
	cliExec := func(step string, words ...string) (json.RawMessage, error) {
		return m.executeRPCScoped(ctx, "cli.exec", append([]string{"--json"}, words...), id+"/"+step, scope)
	}
	// Entry pre-check: applying a weight file that is not installed can only
	// end in a failed probe after a full model reload; refuse before touching
	// the runtime so the user gets an actionable message immediately.
	if dir, dirErr := m.modelDir(ctx); dirErr == nil {
		if _, ok := installedVariants(dir)[model]; !ok {
			return nil, fmt.Errorf(i18n.Message49be01b2b024cf0c, model, strings.ToUpper(engine))
		}
		if engine == "cpu" && model == "quantized" && !m.cpuContainerPresent(dir) {
			return nil, errors.New(i18n.Message7912f8b61b847521)
		}
	}
	// Read the previous selection first: on failure this is exactly what gets
	// restored, so a fresh install without inference.json falls back to the
	// runtime defaults (CPU · quantized) it would have used anyway.
	previous := m.readInferenceSelection()
	prevEngine := previous.Engine
	if prevEngine == "" {
		prevEngine = "cpu"
	}
	prevModel := previous.Model
	if prevModel == "" {
		prevModel = defaultVariantFor(prevEngine)
	}
	// Persisting the choice clears the runtime's loaded-model cache, so the
	// probe below reloads with the selected engine and weight file.
	if receipt, err := cliExec("select", "model", "engine", "--model="+model, "--", engine); err != nil {
		return receipt, err
	}
	probe, probeErr := cliExec("probe", "model", "probe")
	if probeErr == nil {
		return engineEnvelope(engine, model, false, "", probe), nil
	}
	var unknown *unconfirmedError
	if errors.As(probeErr, &unknown) {
		return probe, probeErr
	}
	// The selected engine failed real inference. Restore the previous
	// selection, verify the restore with a probe and report both results.
	if receipt, err := cliExec("restore", "model", "engine", "--model="+prevModel, "--", prevEngine); err != nil {
		return receipt, fmt.Errorf(i18n.Message883ead65e697674c,
			strings.ToUpper(engine), model, probeErr, strings.ToUpper(prevEngine), prevModel, err)
	}
	fbProbe, err := cliExec("restore-probe", "model", "probe")
	if err != nil {
		return fbProbe, fmt.Errorf(i18n.Message359e6792b6118d4b,
			strings.ToUpper(engine), model, probeErr, strings.ToUpper(prevEngine), prevModel, err)
	}
	return engineEnvelope(prevEngine, prevModel, true, probeErr.Error(), fbProbe), nil
}

func (m *Manager) switchManagedModel(ctx context.Context, choice, operation string, scope *runtimeScope) (json.RawMessage, error) {
	receipt, err := m.executeRPCScoped(ctx, "model.switch", []string{choice}, operation, scope)
	if err != nil {
		// model.switch deliberately returns a pending envelope; other
		// errors must not trigger a second switch or an engine downgrade.
		var pending *unconfirmedError
		var envelope struct{ Status string }
		if !errors.As(err, &pending) || json.Unmarshal(receipt, &envelope) != nil || envelope.Status != "pending" {
			return receipt, err
		}
	}
	return m.inspectResult(ctx, operation, scope, errOnlineModelUnconfirmed)
}

// engineEnvelope composes the operation receipt; probe carries the runtime's
// own probe summary (selected execution provider, dims, elapsed time) so the
// UI can show the inference method the runtime actually used. fallback means
// the requested selection failed verification and the previous selection was
// restored; warning then carries the probe failure cause.
func engineEnvelope(engine, model string, fallback bool, warning string, probe json.RawMessage) json.RawMessage {
	summary := map[string]any{"engine": engine, "model": model, "fallback": fallback}
	if warning != "" {
		summary["warning"] = warning
	}
	var probeEnvelope struct {
		Summary json.RawMessage `json:"summary"`
	}
	if json.Unmarshal(probe, &probeEnvelope) == nil && len(probeEnvelope.Summary) > 0 {
		summary["probe"] = probeEnvelope.Summary
	}
	raw, err := json.Marshal(map[string]any{
		"status": "ok", "summary": summary, "items": []any{}, "details": map[string]any{}, "errors": []any{},
	})
	if err != nil {
		raw, _ = json.Marshal(map[string]any{"status": "ok", "summary": summary})
	}
	return raw
}
