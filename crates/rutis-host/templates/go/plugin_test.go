package {{package}}

import (
	"context"
	"testing"

	"github.com/arcships/rutis/go/rutis/rutistest"
)

func TestGreeterGreets(t *testing.T) {
	loaded := rutistest.Load(t, Plugin, Config{Greeting: "Hi"}, nil)
	var greeting string
	if err := loaded.Service("greeter").Call(context.Background(), "hello", &greeting, "Ada"); err != nil {
		t.Fatal(err)
	}
	if greeting != "Hi, Ada!" {
		t.Fatalf("got %q", greeting)
	}
}
