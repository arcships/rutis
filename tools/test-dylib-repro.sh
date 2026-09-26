#!/usr/bin/env bash
set -euo pipefail

repo_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
base="$(mktemp -d /tmp/rutis-dylib-repro.XXXXXX)"
cargo_home="${CARGO_HOME:-$HOME/.cargo}"
for slot in a b; do
  source_dir="$base/$slot/source"
  target_dir="$base/$slot/target"
  mkdir -p "$source_dir"
  tar -C "$repo_dir" --exclude=./target --exclude=./.git -cf - . | tar -C "$source_dir" -xf -
  export CARGO_TARGET_DIR="$target_dir"
  export RUTIS_SDK_LOCKFILE="$source_dir/Cargo.lock"
  export RUSTFLAGS="--remap-path-prefix=$source_dir=/src --remap-path-prefix=$target_dir=/target --remap-path-prefix=$cargo_home=/cargo -C link-arg=-Wl,-rpath,\$ORIGIN"
  cargo build --release --locked --offline -p rutis-sdk --manifest-path "$source_dir/Cargo.toml"
  sha256sum "$target_dir/release/librutis_sdk.so" | cut -d ' ' -f 1 > "$base/$slot.sha"
done
cmp "$base/a.sha" "$base/b.sha"
echo "two independent source and target paths produced the same SDK artifact: $(cat "$base/a.sha")"
base_flags="$RUSTFLAGS"
export RUSTFLAGS="$base_flags -D warnings"
cargo check --release --locked --offline -p rutis-sdk --manifest-path "$base/b/source/Cargo.toml" > /dev/null
echo "lint-only RUSTFLAGS argument was accepted"
export RUSTFLAGS="$base_flags -C relocation-model=pic"
if cargo check --release --locked --offline -p rutis-sdk --manifest-path "$base/b/source/Cargo.toml" > "$base/unclassified.stdout" 2> "$base/unclassified.stderr"; then
  echo "unclassified RUSTFLAGS argument was accepted" >&2
  exit 1
fi
if ! grep -Fq 'unclassified RUSTFLAGS codegen option: relocation-model' "$base/unclassified.stderr"; then
  cat "$base/unclassified.stderr" >&2
  exit 1
fi
echo "unclassified RUSTFLAGS argument was rejected"
