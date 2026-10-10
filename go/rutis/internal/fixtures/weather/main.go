// The runtime conformance plugin alone in a binary.
package main

import (
	"github.com/arcships/rutis/go/rutis"
	"github.com/arcships/rutis/go/rutis/internal/fixtures/plugins/weather"
)

func main() { rutis.Serve(weather.Plugin) }
