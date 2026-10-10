// Package caller uses `ping` (from another binary) and provides `caller`.
package caller

import (
	"context"

	"github.com/arcships/rutis/go/rutis"
)

type Ping struct {
	Echo  func(ctx context.Context, text string) (string, error)
	Later func(ctx context.Context, text string) (string, error)
}

type Caller struct{ ping Ping }

func (c *Caller) Relay(ctx context.Context, text string) (string, error) {
	return c.ping.Echo(ctx, text)
}

func (c *Caller) RelayLater(ctx context.Context, text string) (string, error) {
	return c.ping.Later(ctx, text)
}

var Plugin = rutis.Define(rutis.Plugin[rutis.NoConfig]{
	Name:     "caller",
	Inject:   []string{"ping"},
	Provides: rutis.Provides{"caller": rutis.MethodsOf[*Caller](rutis.Sync("Relay"))},
	Apply: func(ctx *rutis.Ctx, _ rutis.NoConfig) error {
		var ping Ping
		if err := ctx.Use("ping", &ping); err != nil {
			return err
		}
		ctx.Provide("caller", &Caller{ping: ping})
		return nil
	},
})
