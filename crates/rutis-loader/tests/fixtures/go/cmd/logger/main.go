// The lease test plugin (crates/rutis-loader/tests/leases.rs): it writes
// each start and cleanup to the log its config names.
package main

import (
	"context"
	"fmt"
	"os"
	"time"

	"github.com/arcships/rutis/go/rutis"
)

type Config struct {
	Who         string `json:"who"`
	StopDelayMs int    `json:"stopDelayMs,omitempty"`
	Log         string `json:"log"`
}

func write(path, line string) error {
	file, err := os.OpenFile(path, os.O_APPEND|os.O_CREATE|os.O_WRONLY, 0o644)
	if err != nil {
		return err
	}
	defer file.Close()
	_, err = fmt.Fprintln(file, line)
	return err
}

var Plugin = rutis.Define(rutis.Plugin[Config]{
	Name: "logger",
	Apply: func(ctx *rutis.Ctx, config Config) error {
		ctx.Effect(func(context.Context) error {
			time.Sleep(time.Duration(config.StopDelayMs) * time.Millisecond)
			return write(config.Log, "stop "+config.Who)
		})
		return write(config.Log, "start "+config.Who)
	},
})

func main() { rutis.Serve(Plugin) }
