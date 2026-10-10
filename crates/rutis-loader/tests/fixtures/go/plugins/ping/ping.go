// Package ping provides the `ping` service.
package ping

import (
	"context"
	"os"

	"github.com/arcships/rutis/go/rutis"
)

type Ping struct{ from string }

func (p *Ping) Echo(text string) string { return p.from + " pong " + text }

func (p *Ping) Later(ctx context.Context, text string) string { return p.from + " later " + text }

func (p *Ping) Crash() { os.Exit(17) }

// Named is a plugin `name` providing the service `service`, answering as
// `from`.
func Named(name, service, from string) *rutis.Definition {
	return rutis.Define(rutis.Plugin[rutis.NoConfig]{
		Name:     name,
		Provides: rutis.Provides{service: rutis.MethodsOf[*Ping](rutis.Sync("Echo", "Crash"))},
		Apply: func(ctx *rutis.Ctx, _ rutis.NoConfig) error {
			ctx.Provide(service, &Ping{from: from})
			return nil
		},
	})
}
