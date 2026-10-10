package main

import (
	"example.com/fixtures/plugins/caller"

	"github.com/arcships/rutis/go/rutis"
)

func main() { rutis.Serve(caller.Plugin) }
