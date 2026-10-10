#!/usr/bin/env bash
# Run TLC on the configurations of this directory and compare each outcome
# with expected.txt.
#   TLA2TOOLS=/path/to/tla2tools.jar ./run.sh [name ...]
# Needs Java 11+ on PATH (for example `mise exec java@temurin-21 -- ./run.sh`).
# tla2tools.jar: https://github.com/tlaplus/tlaplus/releases/tag/v1.7.4
# (do not commit it).
set -u
cd "$(dirname "$0")"
jar="${TLA2TOOLS:?set TLA2TOOLS to the path of tla2tools.jar}"
out="${TLC_OUT:-$(mktemp -d)}"
mkdir -p "$out"
names=("$@")
if [ ${#names[@]} -eq 0 ]; then
  names=()
  for cfg in *.cfg; do names+=("${cfg%.cfg}"); done
fi
status=0
for name in "${names[@]}"; do
  log="$out/$name.log"
  java -XX:+UseParallelGC -cp "$jar" tlc2.TLC -workers auto -metadir "$out/$name.states" \
    -config "$name.cfg" CrossRuntimeSync.tla > "$log" 2>&1
  if grep -q '^Error: Deadlock reached' "$log"; then
    got=deadlock
  elif inv=$(grep -oE '^Error: Invariant [A-Za-z]+ is violated' "$log"); then
    got=$(echo "$inv" | awk '{print "violated:" $3}')
  elif grep -q '^Model checking completed. No error has been found' "$log"; then
    got=ok
  else
    got=failed
  fi
  want=$(awk -v n="$name" '$1 == n {print $2}' expected.txt)
  states=$(grep -oE '[0-9,]+ distinct states found' "$log" | tail -1)
  printf '%-24s %-34s expected %-34s (%s; log %s)\n' "$name" "$got" "${want:-?}" "$states" "$log"
  [ "$got" = "$want" ] || status=1
done
exit $status
