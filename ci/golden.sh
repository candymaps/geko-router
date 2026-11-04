#!/usr/bin/env bash
# The golden routes of the country packs (countries/<country>/golden/), on frozen data:
#
#   ci/golden.sh [country…]              every pack that has them, or those named
#   ci/golden.sh --write [country…]      writes down what the routes take now: review the diff
#
# The data are the files that golden/snapshot names with their sha256, in $GEKO_GOLDEN_DATA (by
# default /var/lib/geko/golden/router). They are not in the repository, nor published yet: they live
# on the machine that froze them, and a file with another sha256 stops the check, because then a
# difference would say nothing about the code.
set -euo pipefail
cd "$(dirname "$0")/.."

data=${GEKO_GOLDEN_DATA:-/var/lib/geko/golden/router}
write=()
if [ "${1:-}" = --write ]; then
    write=(--write)
    shift
fi
countries=("$@")
if [ ${#countries[@]} -eq 0 ]; then
    for dir in countries/*/golden/; do
        countries+=("$(basename "$(dirname "$dir")")")
    done
fi

cargo build --profile ci --locked -p geko-module-router --example golden
target=${CARGO_TARGET_DIR:-target}
for country in "${countries[@]}"; do
    pack=countries/$country
    awk '!/^#/ && NF { print $2 "  " $1 }' "$pack/golden/snapshot" | (cd "$data" && sha256sum --quiet -c -)
    "$target/ci/examples/golden" "$pack" "$data" ${write[@]+"${write[@]}"}
done
