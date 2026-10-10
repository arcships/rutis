# rutis for Go

Write [rutis](https://github.com/arcships/rutis) plugins in Go; the runtime that runs them is in the same module, so a plugin binary needs nothing else.

```go
package weather

import (
	"context"

	"github.com/arcships/rutis/go/rutis"
)

type LLM struct {
	Ask func(ctx context.Context, question string) (string, error)
}

type Weather struct {
	llm  LLM
	city string
}

func (w *Weather) Today(ctx context.Context) (string, error) {
	answer, err := w.llm.Ask(ctx, "weather in "+w.city)
	return answer + " in " + w.city, err
}

type Config struct {
	City string `json:"city,omitempty"`
}

var Plugin = rutis.Define(rutis.Plugin[Config]{
	Name:     "weather",
	Inject:   []string{"llm"},
	Provides: rutis.Provides{"weather": rutis.MethodsOf[*Weather]()},
	Apply: func(ctx *rutis.Ctx, config Config) error {
		var llm LLM
		if err := ctx.Use("llm", &llm); err != nil {
			return err
		}
		ctx.Provide("weather", &Weather{llm: llm, city: config.City})
		return nil
	},
})
```

A binary serves one or more plugins, and is what a host runs:

```go
func main() { rutis.Serve(weather.Plugin) }
```

Test it without a host (`rutistest` runs the plugin in its runtime, across a real session):

```go
loaded := rutistest.Load(t, weather.Plugin, weather.Config{City: "Oslo"}, map[string]any{"llm": fakeLLM{}})
var today string
err := loaded.Service("weather").Call(context.Background(), "today", &today)
```

Start a project with `rutis-host new <name> --lang go`. Guide: [docs/guide/go-plugin.en.md](https://github.com/arcships/rutis/blob/main/docs/guide/go-plugin.en.md) (中文: [go-plugin.md](https://github.com/arcships/rutis/blob/main/docs/guide/go-plugin.md)). Go 1.24 or later; standard library only.
