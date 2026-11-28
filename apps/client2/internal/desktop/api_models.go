package desktop

import (
	"context"
	"encoding/json"
	"errors"
	"io"
	"net/http"
	"time"

	"respire-client2/internal/host"
	"respire-client2/internal/i18n"
)

func modelActionStatus(err error) int {
	switch {
	case errors.Is(err, host.ErrInvalidModelVariant):
		return http.StatusBadRequest
	case errors.Is(err, host.ErrModelFilesMissing):
		return http.StatusNotFound
	case errors.Is(err, host.ErrModelVariantBusy):
		return http.StatusConflict
	default:
		return http.StatusInternalServerError
	}
}

func (d *desktop) modelActionRoute(w http.ResponseWriter, r *http.Request) bool {
	probe := r.URL.Path == "/api/model-probe"
	if !probe && r.URL.Path != "/api/model-delete" {
		return false
	}
	encode := json.NewEncoder(w)
	writeError := func(status int, err error) {
		w.WriteHeader(status)
		encode.Encode(map[string]any{"ok": false, "error": err.Error()})
	}
	if r.Method != http.MethodPost || r.Header.Get("X-Respire-Client") != "1" {
		writeError(http.StatusForbidden, errors.New(i18n.Message0a8050e64dca06f2))
		return true
	}
	var body struct {
		Model   string `json:"model"`
		Variant string `json:"variant"`
		Engine  string `json:"engine"`
	}
	decoder := json.NewDecoder(http.MaxBytesReader(w, r.Body, 4096))
	decoder.DisallowUnknownFields()
	if err := decoder.Decode(&body); err != nil {
		writeError(http.StatusBadRequest, err)
		return true
	}
	var trailing any
	if decoder.Decode(&trailing) != io.EOF {
		writeError(http.StatusBadRequest, errors.New(i18n.Messaged8d27c196a37710e))
		return true
	}
	timeout := 30 * time.Second
	if probe {
		timeout = host.ModelProbeTimeout
	}
	ctx, cancel := context.WithTimeout(r.Context(), timeout)
	defer cancel()
	var result any
	var err error
	if probe {
		result, err = d.m.ProbeModelVariant(ctx, body.Model, body.Variant, body.Engine)
	} else {
		var variant string
		variant, err = d.m.DeleteModelVariant(ctx, body.Model, body.Variant)
		result = map[string]string{"model": body.Model, "variant": variant, "status": "deleted"}
	}
	if err != nil {
		writeError(modelActionStatus(err), err)
	} else {
		encode.Encode(map[string]any{"ok": true, "result": result})
	}
	return true
}
