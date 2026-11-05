#!/usr/bin/env bash
# The golden routes of the country packs (countries/<country>/golden/), on frozen data:
#
#   ci/golden.sh [country…]              every pack that has them, or those named
#   ci/golden.sh --write [country…]      writes down what the routes take now: review the diff
#   ci/golden.sh --publish [country…]    pushes the frozen data to the registry (private)
#
# The data are the files that golden/snapshot names with their sha256, in $GEKO_GOLDEN_DATA (by
# default /var/lib/geko/golden/router). Those that are not there are pulled from
# $GEKO_GOLDEN_REGISTRY:<country>-<hash of golden/snapshot>, by default in
# ghcr.io/candymaps/geko-router/golden: `oras login ghcr.io` with read:packages, or with
# write:packages to publish. A file with another sha256 stops the check, because then a difference
# would say nothing about the code. New data are published before committing their golden/snapshot.
set -euo pipefail
cd "$(dirname "$0")/.."
# shellcheck source=ci/oras.sh
. ci/oras.sh

data=${GEKO_GOLDEN_DATA:-/var/lib/geko/golden/router}
registry=${GEKO_GOLDEN_REGISTRY:-ghcr.io/candymaps/geko-router/golden}

# What golden/snapshot of the country `$1` names: the lines that are not comments.
entries() { grep -v '^#' "countries/$1/golden/snapshot" | grep .; }
files() { entries "$1" | cut -d' ' -f1; }
check() { entries "$1" | awk '{ print $2 "  " $1 }' | (cd "$data" && sha256sum --quiet -c -); }
ref() { echo "$registry:$1-$(entries "$1" | sha256sum | cut -c1-12)"; }

pull() {
    local file
    mkdir -p "$data"
    for file in $(files "$1"); do
        if [ ! -f "$data/$file" ]; then
            (cd "$data" && oras pull --no-tty "$(cd - > /dev/null && ref "$1")" > /dev/null)
            return
        fi
    done
}

# Pushes the frozen data of a country, checked by nothing but their sha256 in the repository. Their
# package is linked to the repository, so that the CI reads it with its own token; it is created
# private, and stays so.
publish() {
    local to names
    to=$(ref "$1")
    names=$(files "$1")
    check "$1"
    # shellcheck disable=SC2086
    (cd "$data" && oras push --no-tty "$to" $names \
        --artifact-type application/vnd.geko.golden-snapshot.v1 \
        --annotation "org.opencontainers.image.source=https://github.com/candymaps/geko-router" \
        --annotation "org.opencontainers.image.description=Frozen data of the golden routes (OpenStreetMap ODbL 1.0, © OpenStreetMap contributors; Ayuntamiento de Madrid, datos abiertos, CC BY 4.0)" \
        --annotation "org.opencontainers.image.created=1970-01-01T00:00:00Z" > /dev/null)
    echo "published $to"
}

mode=check
case "${1:-}" in
    --write) mode=write; shift ;;
    --publish) mode=publish; shift ;;
esac
countries=("$@")
if [ ${#countries[@]} -eq 0 ]; then
    for dir in countries/*/golden/; do
        countries+=("$(basename "$(dirname "$dir")")")
    done
fi

if [ "$mode" = publish ]; then
    for country in "${countries[@]}"; do
        publish "$country"
    done
    exit 0
fi

write=()
[ "$mode" = check ] || write=(--write)
cargo build --profile ci --locked -p geko-module-router --example golden
target=${CARGO_TARGET_DIR:-target}
for country in "${countries[@]}"; do
    pull "$country"
    check "$country"
    "$target/ci/examples/golden" "countries/$country" "$data" ${write[@]+"${write[@]}"}
done
