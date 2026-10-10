package convert

import (
	"context"
	"encoding/json"
	"reflect"
	"testing"

	"github.com/arcships/rutis/go/rutis/internal/peer"
)

func TestWireName(t *testing.T) {
	for goName, wire := range map[string]string{
		"Today": "today", "ID": "id", "URLFor": "urlFor", "HTTPServer": "httpServer",
		"GetURL": "getURL", "ListModels": "listModels", "ID2": "id2", "already": "already",
	} {
		if got := WireName(goName); got != wire {
			t.Errorf("WireName(%q) = %q, want %q", goName, got, wire)
		}
	}
}

type point struct {
	X int `json:"x"`
	Y int `json:"y"`
}

func TestDataCrossesAsJSON(t *testing.T) {
	value, err := ToPeer(point{1, 2})
	if err != nil {
		t.Fatal(err)
	}
	if string(value.(json.RawMessage)) != `{"x":1,"y":2}` {
		t.Fatalf("%s", value)
	}
	var back point
	if err := Decode(value, &back); err != nil || back != (point{1, 2}) {
		t.Fatalf("%v %v", back, err)
	}
	if _, err := ToPeer(int64(1) << 60); err == nil {
		t.Fatal("an integer beyond 2^53 must not cross")
	}
	if _, err := ToPeer([]byte("x")); err == nil {
		t.Fatal("bytes must not cross")
	}
	if _, err := ToPeer(map[int]string{1: "a"}); err == nil {
		t.Fatal("non-string keys must not cross")
	}
	if _, err := ToPeer(struct{ F func() }{func() {}}); err == nil {
		t.Fatal("a struct is data: a function field cannot cross")
	}
}

func TestFunctionsCrossAndAreCalledWithWireArguments(t *testing.T) {
	value, err := ToPeer(func(ctx context.Context, a int, b string) (string, error) {
		return b + string(rune('0'+a)), nil
	})
	if err != nil {
		t.Fatal(err)
	}
	fn, ok := value.(peer.Func)
	if !ok {
		t.Fatalf("a Go function crosses as a peer.Func, not %T", value)
	}
	result, err := fn(context.Background(), []any{json.RawMessage("3"), json.RawMessage(`"n"`)})
	if err != nil {
		t.Fatal(err)
	}
	out := []reflect.Value{reflect.ValueOf(result)}
	if got := string(out[0].Interface().(json.RawMessage)); got != `"n3"` {
		t.Fatalf("%s", got)
	}
}

func TestCallStripsTrailingUndefinedAndRejectsExtraArguments(t *testing.T) {
	fn := reflect.ValueOf(func(a int) int { return a + 1 })
	if _, err := Call(context.Background(), fn, []any{json.RawMessage("1"), json.RawMessage("2")}); err == nil {
		t.Fatal("extra arguments are an error")
	}
	value, err := Call(context.Background(), fn, nil)
	if err != nil || string(value.(json.RawMessage)) != "1" {
		t.Fatalf("a missing argument is its zero value: %v %v", value, err)
	}
}

func TestPanicsBecomeErrors(t *testing.T) {
	_, err := Call(context.Background(), reflect.ValueOf(func() { panic("boom") }), nil)
	if err == nil || err.Error() != "panic: boom" {
		t.Fatalf("%v", err)
	}
}
