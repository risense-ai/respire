package host

import (
	"errors"
	"io"
	"testing"
)

func TestOnlineModelReceiptFailsClosed(t *testing.T) {
	cases := []struct {
		name, reply string
		expected    error
	}{
		{"confirmed", `{"http_status":200,"body":{"model_id":"m3","outcome":"removed","active_changed":false,"detached_files":2,"deleted_files":2,"cleanup_error":null}}`, nil},
		{"active", `{"http_status":409,"body":{"code":"model_in_use"}}`, errM3Active},
		{"unknown", `{"http_status":409,"body":{"code":"managed_model_unavailable"}}`, errOnlineModelUnavailable},
		{"other", `{"http_status":200,"body":{"model_id":"different","outcome":"removed","active_changed":false,"detached_files":2,"deleted_files":2}}`, errOnlineModelUnconfirmed},
		{"changed", `{"http_status":200,"body":{"model_id":"m3","outcome":"removed","active_changed":true,"detached_files":2,"deleted_files":2}}`, errOnlineModelUnconfirmed},
		{"incomplete", `{"http_status":200,"body":{"model_id":"m3","outcome":"removed","detached_files":2,"deleted_files":2}}`, errOnlineModelUnconfirmed},
		{"detached", `{"http_status":200,"body":{"model_id":"m3","outcome":"cleanup_pending","active_changed":false,"detached_files":2,"deleted_files":1,"cleanup_error":"in progress"}}`, errOnlineModelUnconfirmed},
	}
	for _, one := range cases {
		t.Run(one.name, func(t *testing.T) {
			receipt, err := decodeOnlineModelReceipt([]byte(one.reply), "m3")
			if !errors.Is(err, one.expected) {
				t.Fatalf("got %v, want %v", err, one.expected)
			}
			if one.expected == nil && receipt.Deleted != 2 {
				t.Fatalf("lost the removal receipt: %+v", receipt)
			}
		})
	}
}

func TestOnlineModelOutputBound(t *testing.T) {
	out := &onlineModelOutput{}
	if _, err := out.Write(make([]byte, 8191)); err != nil {
		t.Fatal(err)
	}
	if _, err := out.Write(make([]byte, 2)); !errors.Is(err, io.ErrShortBuffer) || out.buffer.Len() != 8191 {
		t.Fatalf("unexpected oversized frame result: %v", err)
	}
}
