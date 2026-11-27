package host

import (
	"bufio"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"strings"
	"time"

	runtimesdk "github.com/risense-ai/respire/packages/runtime-sdk-go"
)

type runtimeTerminalError struct{ state, detail string }

func (e *runtimeTerminalError) Error() string { return e.detail }
func (e *runtimeTerminalError) Unwrap() error {
	if e.state == "cancelled" {
		return context.Canceled
	}
	return nil
}

// The SDK owns the request and event DTOs; this parser only handles SSE framing.
func parseOperationEvents(reader io.Reader, id string, receive func(runtimesdk.OperationEvent) error) error {
	readerBuffer := bufio.NewReader(reader)
	var eventName string
	var data []string
	for {
		line, readErr := readerBuffer.ReadString('\n')
		if readErr != nil && readErr != io.EOF {
			return readErr
		}
		line = strings.TrimSuffix(strings.TrimSuffix(line, "\n"), "\r")
		if line == "" && len(data) > 0 {
			var event runtimesdk.OperationEvent
			if err := json.Unmarshal([]byte(strings.Join(data, "\n")), &event); err != nil {
				return err
			}
			if event.Data.OperationId != id || string(event.Event) != eventName || !event.Data.State.Valid() {
				return errors.New("Runtime operation event does not match its subscription")
			}
			terminal := false
			switch eventName {
			case "progress":
				if event.Data.State != "pending" && event.Data.State != "running" {
					return errors.New("Runtime progress event has a terminal state")
				}
			case "completed", "failed", "cancelled":
				terminal = true
				if string(event.Data.State) != eventName {
					return errors.New("Runtime terminal event has an inconsistent state")
				}
			default:
				return fmt.Errorf("Unknown Runtime operation event: %s", eventName)
			}
			if err := receive(event); err != nil {
				return err
			}
			if terminal {
				return nil
			}
			data, eventName = nil, ""
		} else if field, value, ok := strings.Cut(line, ":"); ok {
			value = strings.TrimPrefix(value, " ")
			switch field {
			case "event":
				eventName = value
			case "data":
				data = append(data, value)
			}
		} else if line == "" {
			eventName = ""
		}
		if readErr == io.EOF {
			return io.ErrUnexpectedEOF
		}
	}
}

func (m *Manager) StreamRuntimeOperation(ctx context.Context, id string, receive func(runtimesdk.OperationEvent) error) error {
	_, supported, err := m.runtimeSDK(ctx, "sse.operations.v1")
	if err != nil {
		return err
	}
	if !supported {
		return errors.New("Runtime does not advertise sse.operations.v1")
	}
	// A stream outlives the normal JSON request timeout; its context owns its lifetime.
	httpClient := *m.localClient
	httpClient.Timeout = 0
	client, err := runtimesdk.NewClient(m.Endpoint, runtimesdk.WithHTTPClient(&httpClient))
	if err != nil {
		return err
	}
	response, err := client.StreamOperationEvents(ctx, id, m.authorizeRuntime)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	mediaType, _, err := mime.ParseMediaType(response.Header.Get("Content-Type"))
	if response.StatusCode != http.StatusOK || err != nil || mediaType != "text/event-stream" {
		return fmt.Errorf("Runtime SSE rejected: HTTP %d, content type %q", response.StatusCode, mediaType)
	}
	return parseOperationEvents(response.Body, id, receive)
}

func (m *Manager) waitRuntimeOperation(ctx context.Context, accepted runtimesdk.OperationAccepted, stream bool, receive func(runtimesdk.OperationEvent) error) error {
	consume := func(event runtimesdk.OperationEvent) error {
		if receive != nil {
			if err := receive(event); err != nil {
				return err
			}
		}
		switch event.Event {
		case "failed":
			return &runtimeTerminalError{"failed", fmt.Sprintf("Runtime operation failed: %v", event.Data.Error)}
		case "cancelled":
			return &runtimeTerminalError{"cancelled", "Runtime operation cancelled"}
		}
		return nil
	}
	if stream {
		return m.StreamRuntimeOperation(ctx, accepted.OperationId, consume)
	}
	client, supported, err := m.runtimeSDK(ctx, "openapi.v1")
	if err != nil || !supported {
		return errors.Join(err, errors.New("Runtime operation snapshots are unavailable"))
	}
	for {
		raw, err := sdkJSON(client.GetOperation(ctx, accepted.OperationId, m.authorizeRuntime))
		if err != nil {
			return err
		}
		var status runtimesdk.OperationStatus
		if err := json.Unmarshal(raw, &status); err != nil {
			return err
		}
		if status.OperationId != accepted.OperationId || !status.State.Valid() {
			return errors.New("Runtime operation snapshot is invalid")
		}
		event := "progress"
		if status.State != "pending" && status.State != "running" {
			event = string(status.State)
		}
		if err := consume(runtimesdk.OperationEvent{Event: runtimesdk.OperationEventEvent(event), Data: status}); err != nil {
			return err
		}
		if event != "progress" {
			return nil
		}
		select {
		case <-ctx.Done():
			return ctx.Err()
		case <-time.After(time.Second):
		}
	}
}
