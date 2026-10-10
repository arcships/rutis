// netkit, rebuilt with one more plugin.
package main

import (
	"example.com/fixtures/plugins/ping"

	"github.com/arcships/rutis/go/rutis"
)

func main() {
	rutis.Serve(ping.Named("ping", "ping", "netkit2"), ping.Named("extra", "extra", "extra"))
}
