package peer

import (
	"encoding/json"
	"errors"
	"fmt"
	"reflect"
	"strings"
)

// RemoteError is an error thrown on the other side of the session.
type RemoteError struct {
	Name    string
	Message string
	Graph   json.RawMessage
}

func (e *RemoteError) Error() string {
	if e.Name == "" {
		return e.Message
	}
	return e.Name + ": " + e.Message
}

// Is makes errors.Is(err, ErrSyncWaitCycle) hold for a received cycle.
func (e *RemoteError) Is(target error) bool {
	return target == ErrSyncWaitCycle && e.Name == "SyncWaitCycle"
}

// ErrSyncWaitCycle matches a SyncWaitCycle error received from the far end.
var ErrSyncWaitCycle = errors.New("SyncWaitCycle")

// PanicError is a recovered panic, thrown with its stack.
type PanicError struct {
	Value any
	Stack string
}

func (e *PanicError) Error() string { return fmt.Sprintf("panic: %v", e.Value) }
func (e *PanicError) Name() string  { return "Panic" }

// ErrorName is the name an error crosses with: Name() when it has one, the
// far end's name for a RemoteError, "Error" for the standard library's
// plain errors, else its type name (*fs.PathError -> PathError).
func ErrorName(err error) string {
	var remote *RemoteError
	if errors.As(err, &remote) && err == error(remote) {
		return remote.Name
	}
	if named, ok := err.(interface{ Name() string }); ok {
		return named.Name()
	}
	t := reflect.TypeOf(err)
	for t.Kind() == reflect.Pointer {
		t = t.Elem()
	}
	switch t.PkgPath() + "." + t.Name() {
	case "errors.errorString", "fmt.wrapError", "fmt.wrapErrors", "errors.joinError":
		return "Error"
	}
	if t.Name() == "" {
		return "Error"
	}
	return t.Name()
}

func encodeError(err error) map[string]any {
	var remote *RemoteError
	if errors.As(err, &remote) && err == error(remote) && remote.Name == "SyncWaitCycle" && remote.Graph == nil {
		return map[string]any{"name": "SyncWaitCycle", "message": remote.Message}
	}
	name := ErrorName(err)
	message := err.Error()
	if err == error(remote) {
		message = remote.Message
	}
	stack := name + ": " + message
	var panicked *PanicError
	if errors.As(err, &panicked) {
		stack += "\n" + panicked.Stack
	}
	node := map[string]any{"type": "error", "name": name, "message": message, "stack": stack}
	return map[string]any{
		"name":    name,
		"message": message,
		"graph":   map[string]any{"root": map[string]any{"type": "reference", "value": 0}, "nodes": []any{node}},
	}
}

func decodeError(raw json.RawMessage) error {
	var failure struct {
		Name    any             `json:"name"`
		Message any             `json:"message"`
		Graph   json.RawMessage `json:"graph"`
	}
	if err := json.Unmarshal(raw, &failure); err != nil {
		return &RemoteError{Name: "Error", Message: "malformed error"}
	}
	name, message := "Error", ""
	if failure.Name != nil {
		name = fmt.Sprint(failure.Name)
	}
	if failure.Message != nil {
		message = fmt.Sprint(failure.Message)
	}
	graph := failure.Graph
	if strings.TrimSpace(string(graph)) == "null" {
		graph = nil
	}
	return &RemoteError{Name: name, Message: message, Graph: graph}
}
