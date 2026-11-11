# Kani and Miri harnesses (#631)

Both are their own workspaces, like `fuzz/`: plain `cargo build` and
`cargo test` in the repository never build them.

## Miri

Miri is installed for the `nightly` toolchain here (0.1.0). The harness
runs the session protocol's pure frame and value path on embedded
fixtures; it reads no files, clocks or network, so Miri's default
isolation is enough.

```
RUSTFLAGS="--cfg fuzzing" \
    cargo +nightly miri run --manifest-path fuzz/miri/Cargo.toml
```

`--cfg fuzzing` exposes `rutis_bridge::fuzzing::session_frame`, the entry
point of the `session_frame` cargo-fuzz target.

The property tests in `crates/rutis-bridge/src/session/protocol.rs` also
run under Miri, with a small case count (proptest reads the clock, so
isolation is off):

```
MIRIFLAGS=-Zmiri-disable-isolation PROPTEST_CASES=2 \
    cargo +nightly miri test -p rutis-bridge --features testing --lib \
    session::protocol::tests::frames_write_and_read_back_unchanged -- --test-threads=1
MIRIFLAGS=-Zmiri-disable-isolation PROPTEST_CASES=2 \
    cargo +nightly miri test -p rutis-bridge --features testing --lib \
    session::protocol::tests::wire_values_write_and_read_back_unchanged -- --test-threads=1
```

## Kani

`cargo-kani` is not installed here, and the `kani-0.68.0` release download
(140 MB) stalled at ~33 kB/s; the harness has not been run. With Kani
installed (`cargo install --locked kani-verifier && cargo kani setup`):

```
cd fuzz/kani && cargo kani
```

The proofs cover the date and time-of-day constructors and the
days-from-epoch round trip in
`crates/rutis-bridge/src/session/values.rs`.

## Results (2026-11-11)

Miri 0.1.0, nightly 1.98.1 host, 32 cores:

| Command | Result |
| --- | --- |
| `cargo +nightly miri run --manifest-path fuzz/miri/Cargo.toml` (with `RUSTFLAGS=--cfg fuzzing`), 13 fixtures + one frame past the depth limit | 15 lines of output, `miri: ok`, no UB, no panic |
| `cargo +nightly miri test -p rutis-bridge --features testing --lib session::protocol::tests::frames_write_and_read_back_unchanged -- --test-threads=1` (`PROPTEST_CASES=2`, `MIRIFLAGS=-Zmiri-disable-isolation`) | 1 passed, 122 s |
| same, `wire_values_write_and_read_back_unchanged` | 1 passed, 31 s |
| `cargo +nightly miri test -p rutis-bridge --features testing --lib session::rpc::waits -- --test-threads=1` (`MIRIFLAGS=-Zmiri-disable-isolation`) | 15 passed, 1 ignored (`measure_the_check`, a measurement), 846 s |

Kani: not run, no result. `cargo kani --version` says `no such command:
kani`; `cargo install --locked kani-verifier` would download 140 MB from
the Kani GitHub release, which stalled at ~33 kB/s here and was stopped.
The harness is therefore unproven: do not report these proofs as passing
until `cargo kani` has run.
