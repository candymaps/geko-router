#!/usr/bin/env bash
# The country packs of this repository, with only what the router reads at run time: pack.toml,
# data/ and the sources/*.toml declarations. The tests and the golden routes stay behind.
#
#   ci/packs.sh <folder>                copies every pack to <folder>/<country>/
#   ci/packs.sh publish [country…]      signs every pack (or those named) and pushes those that
#                                       changed to the registry, as OCI artifacts
#
# Publishing needs GEKO_PACK_SIGNING_KEY (hex; the key of the other artifacts of geko) and a login
# to the registry with permission to push. Each pack goes to $GEKO_PACKS_REGISTRY/<country>, by
# default ghcr.io/candymaps/geko-router/countries/<country>, with the tag of the format the module
# reads (format-<n>), which each geko downloads, pack-<n> with the version of the pack (pack.toml),
# and <version>-<commit>, the version being that of Cargo.toml. A pack that changed under a version
# already published with other files is refused, and then nothing is pushed: raise its version.
# GEKO_PACKS_DRY_RUN=1 signs and checks the packs and stops before the registry, and with a key of
# test it leaves the check against the trusted keys out. `oras` runs from its image when it is not
# installed.
set -euo pipefail
cd "$(dirname "$0")/.."

copy() {
    local out=$1 dir country
    for dir in countries/*/; do
        [ -f "$dir/pack.toml" ] || continue
        country=$(basename "$dir")
        [ $# -le 1 ] || [[ " ${*:2} " == *" $country "* ]] || continue
        rm -rf "${out:?}/$country"
        mkdir -p "$out/$country"
        cp "$dir/pack.toml" "$out/$country/"
        cp -r "$dir/data" "$out/$country/"
        if compgen -G "$dir/sources/*.toml" > /dev/null; then
            mkdir -p "$out/$country/sources"
            cp "$dir"/sources/*.toml "$out/$country/sources/"
        fi
    done
}

# shellcheck source=ci/oras.sh
. ci/oras.sh

# Whether the artifact at `$2` holds exactly the files of the folder `$1`: publishing it again
# would change nothing, and every geko would download it again for nothing.
same() {
    local manifest
    manifest=$(oras manifest fetch "$2" 2> /dev/null) || return 1
    python3 -c '
import hashlib, json, os, sys
folder, manifest = sys.argv[1], json.loads(sys.argv[2])
remote = {l.get("annotations", {}).get("org.opencontainers.image.title"): l["digest"] for l in manifest.get("layers", [])}
local = {}
for root, _, files in os.walk(folder):
    for name in files:
        path = os.path.join(root, name)
        local[os.path.relpath(path, folder)] = "sha256:" + hashlib.sha256(open(path, "rb").read()).hexdigest()
sys.exit(0 if remote == local else 1)
' "$1" "$manifest"
}

publish() {
    : "${GEKO_PACK_SIGNING_KEY:?GEKO_PACK_SIGNING_KEY is not set}"
    local registry=${GEKO_PACKS_REGISTRY:-ghcr.io/candymaps/geko-router/countries}
    local target=${CARGO_TARGET_DIR:-target} format version commit tags work country files ref pack
    local sign=$target/release/geko-router-pack-sign
    local changed=() refused=()
    cargo build --release --locked -p geko-module-router --bin geko-router-pack-sign
    format=$("$sign" format)
    version=$(sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p' Cargo.toml)
    commit=$(git rev-parse --short HEAD)
    tags="$version-$commit"
    work=$(mktemp -d)
    trap 'rm -rf "$work"' RETURN
    copy "$work" "$@"
    for dir in "$work"/*/; do
        country=$(basename "$dir")
        "$sign" sign "$dir"
        pack=$("$sign" version "$dir")
        if [ "${GEKO_PACKS_DRY_RUN:-0}" = 1 ]; then
            echo "signed $country (pack $pack, format $format): $(cd "$dir" && find . -type f | wc -l) files"
            continue
        fi
        "$sign" verify "$dir"
        ref="$registry/$country:format-$format"
        if same "$dir" "$ref"; then
            oras tag "$ref" "pack-$pack" > /dev/null
            echo "unchanged $ref (pack $pack)"
        elif oras manifest fetch "$registry/$country:pack-$pack" > /dev/null 2>&1; then
            refused+=("$country: changed, but version $pack is already published; raise version in countries/$country/pack.toml")
        else
            changed+=("$dir")
        fi
    done
    if [ ${#refused[@]} -gt 0 ]; then
        printf '%s\n' "${refused[@]}" >&2
        return 1
    fi
    for dir in ${changed[@]+"${changed[@]}"}; do
        country=$(basename "$dir")
        pack=$("$sign" version "$dir")
        ref="$registry/$country:format-$format"
        files=$(cd "$dir" && find . -type f | sed 's|^\./||' | sort)
        # Relative paths: oras names each layer after its file, and geko writes it back there. A
        # fixed date, instead of that of the push, so that the same files give the same manifest.
        # shellcheck disable=SC2086
        (cd "$dir" && oras push --disable-path-validation "$ref,pack-$pack,$tags" \
            --artifact-type application/vnd.geko.router-pack.v1 \
            --annotation "org.opencontainers.image.created=1970-01-01T00:00:00Z" $files)
        echo "published $ref (pack-$pack,$tags)"
    done
}

case "${1:?usage: ci/packs.sh <folder> | publish [country…]}" in
    publish) shift; publish "$@" ;;
    *) copy "$1" ;;
esac
