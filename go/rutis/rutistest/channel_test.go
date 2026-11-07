package rutistest

import (
	"io"
	"net"
	"testing"
)

// The far end greets before it reads (#239), so its greeting often arrives in
// the same segment as the token: readTokenLine must leave it in the socket
// for the session. A buffered reader took it too, and the check that refused
// "extra bytes" then closed a perfectly good connection — which is what
// `go test ./...` on Windows hit ("rutis-host: test: peer disconnected") for
// every test tool test.
func TestTheGreetingAfterTheTokenStaysForTheSession(t *testing.T) {
	ours, theirs := net.Pipe()
	defer ours.Close()
	defer theirs.Close()
	go func() {
		// One write: the token line and the greeting the far end sends
		// right after it.
		_, _ = theirs.Write([]byte("a1b2c3d4\n{\"op\":\"hello\"}"))
	}()
	line, ok := readTokenLine(ours)
	if !ok || line != "a1b2c3d4" {
		t.Fatalf("readTokenLine = %q, %v", line, ok)
	}
	rest := make([]byte, len(`{"op":"hello"}`))
	if _, err := readFull(ours, rest); err != nil {
		t.Fatalf("the greeting was swallowed: %v", err)
	}
	if string(rest) != `{"op":"hello"}` {
		t.Fatalf("the greeting = %q", rest)
	}
}

// A connection that sends more than a token line is still refused: it is not
// one this tool started.
func TestALineThatIsNotOneTokenIsRefused(t *testing.T) {
	ours, theirs := net.Pipe()
	defer ours.Close()
	defer theirs.Close()
	go func() { _, _ = theirs.Write([]byte("GET / HTTP/1.0\r\n\r\n")) }()
	if line, ok := readTokenLine(ours); ok {
		t.Fatalf("readTokenLine accepted %q", line)
	}
}

func readFull(conn net.Conn, buffer []byte) (int, error) {
	return io.ReadFull(conn, buffer)
}
