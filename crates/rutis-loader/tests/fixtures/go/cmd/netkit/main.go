package main

import (
	"example.com/fixtures/plugins/ping"

	"github.com/arcships/rutis/go/rutis"
)

func main() { rutis.Serve(ping.Named("ping", "ping", "netkit")) }
