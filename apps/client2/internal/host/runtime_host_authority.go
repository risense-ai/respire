package host

import (
	"context"
	"errors"
	"io"
	"time"
)

var errRuntimeHostAuthorityUnavailable = errors.New("Runtime host authority is unavailable")

type runtimeOwnershipKind uint8

const (
	runtimeTakeoverOwnership runtimeOwnershipKind = iota + 1
	runtimeDirectoryOwnership
)

// This is a Go boundary. The current private control protocol has no equivalent.
type runtimeHostAuthority interface {
	// Ownership excludes CLI/Runtime users of the same lock until Close,
	// including after the acquisition context is cancelled.
	acquire(ctx context.Context, kind runtimeOwnershipKind, root, directory string, wait time.Duration) (io.Closer, error)
}
