// The {{name}} binary: what a host runs.
package main

import (
	"github.com/arcships/rutis/go/rutis"

	{{package}} "example.com/{{name}}"
)

func main() { rutis.Serve({{package}}.Plugin) }
