#!/usr/bin/env bash
# The module of this repository as the package geko downloads: module/, with its library (the crate
# geko-module-router-dylib) compiled in release, and module.toml completed (version, contract,
# platform) and signed.
#
#   ci/modules.sh build <folder>     compiles the module into <folder>/<module>/
#   ci/modules.sh publish <folder>   signs the packages of <folder> and pushes them to
#                                    $GEKO_MODULES_REGISTRY/geko-<module>
#
# The version of the module is that of Cargo.toml, followed by the commit (<version>-<commit>) unless
# GEKO_RELEASE=1; it is compiled into the module (GEKO_MODULE_VERSION), which says it to the host
# and to the bundles it builds. Each package goes to geko-<module>:<version>-<platform> and
# contract-<n>-<platform>, the newest of its contract, which every geko of that contract downloads;
# its manifest says the three in its annotations (geko.version, geko.contract, geko.platform), so
# that geko can refuse one that is not for it before downloading it.
#
# The signing tool, geko-module-sign, comes from the host at the commit of the SDK in Cargo.lock.
# `build` must run on a system as old as the image of
# geko (debian:bookworm-slim): a module that needs a newer glibc does not start in it. The CI runs
# it in rust:<toolchain>-bookworm. Publishing needs GEKO_PACK_SIGNING_KEY (hex), the key of the
# packs, and a login to the registry.
set -euo pipefail
cd "$(dirname "$0")/.."

target=${CARGO_TARGET_DIR:-target}
registry=${GEKO_MODULES_REGISTRY:-ghcr.io/candymaps}

field() { tr -d '\r' < "$2" | sed -n "s/^$1 *= *\"\{0,1\}\([^\"]*\)\"\{0,1\} *\$/\1/p" | head -1; }

sign_tool() {
    local source rev
    # The SDK as Cargo.lock resolved it: git+<url>?tag=sdk-<contract>#<commit>.
    source=$(tr -d '\r' < Cargo.lock | grep -A2 '^name = "geko-module-sdk"$' | sed -n 's/^source = "git+\(.*\)"$/\1/p')
    rev=${source##*#}
    cargo install --locked --quiet --git "${source%%\?*}" --rev "$rev" \
        --root "$target/tools/$rev" geko-artifact --bin geko-module-sign >&2
    echo "$target/tools/$rev/bin/geko-module-sign"
}

version() {
    local version
    # Without the CR of a checkout made on Windows.
    version=$(tr -d '\r' < Cargo.toml | sed -n '/^\[workspace.package\]/,/^\[/s/^version = "\(.*\)"/\1/p')
    if [ "${GEKO_RELEASE:-0}" = 1 ]; then echo "$version"; else echo "$version-$(git rev-parse --short HEAD)"; fi
}

# The contract of the SDK this module is compiled with, read from its sources: `build` runs in a
# container where the signing tool, compiled on the runner, might not start.
contract() {
    local manifest
    manifest=$(cargo metadata --locked --format-version 1 | grep -o '"manifest_path":"[^"]*/geko-module-sdk/Cargo.toml"' | head -1)
    manifest=${manifest#\"manifest_path\":\"}
    sed -n 's/^pub const CONTRACT: u32 = \([0-9]*\);/\1/p' "$(dirname "${manifest%\"}")/src/module.rs"
}

build() {
    local out=$1 contract platform version name kind entry
    contract=$(contract)
    [ -n "$contract" ] || { echo "the contract of the SDK not found" >&2; return 1; }
    # As geko names it (std::env::consts::OS and ARCH), which uname gives on Linux.
    platform="$(uname -s | tr '[:upper:]' '[:lower:]')-$(uname -m)"
    version=$(version)
    name=$(field name module/module.toml)
    kind=$(field kind module/module.toml)
    entry=$(field entry module/module.toml)
    GEKO_MODULE_VERSION=$version cargo build --release --locked -p "geko-module-$name-dylib"
    rm -rf "${out:?}/$name"
    mkdir -p "$out/$name"
    cp "$target/release/$entry" "$out/$name/$entry"
    printf 'kind = "%s"\nentry = "%s"\nname = "%s"\nversion = "%s"\ncontract = %s\nplatform = "%s"\n' \
        "$kind" "$entry" "$name" "$version" "$contract" "$platform" > "$out/$name/module.toml"
    echo "built $name $version (contract $contract, $platform) in $out/$name"
}

# shellcheck source=ci/oras.sh
. ci/oras.sh

publish() {
    : "${GEKO_PACK_SIGNING_KEY:?GEKO_PACK_SIGNING_KEY is not set}"
    local out=$1 tool dir name version contract platform files ref
    tool=$(sign_tool)
    for dir in "$out"/*/; do
        dir=${dir%/}
        name=$(field name "$dir/module.toml")
        version=$(field version "$dir/module.toml")
        contract=$(field contract "$dir/module.toml")
        platform=$(field platform "$dir/module.toml")
        "$tool" sign "$dir"
        "$tool" verify "$dir"
        ref="$registry/geko-$name"
        files=$(cd "$dir" && find . -type f | sed 's|^\./||' | sort)
        # A fixed date, instead of that of the push, so that the same files give the same manifest.
        # shellcheck disable=SC2086
        (cd "$dir" && oras push --disable-path-validation "$ref:$version-$platform,contract-$contract-$platform" \
            --artifact-type application/vnd.geko.module.v1 \
            --annotation "org.opencontainers.image.created=1970-01-01T00:00:00Z" \
            --annotation "geko.version=$version" --annotation "geko.contract=$contract" --annotation "geko.platform=$platform" \
            $files)
        echo "published $ref ($version-$platform, contract-$contract-$platform)"
    done
}

case "${1:-}" in
    build) shift; build "${1:?usage: ci/modules.sh build <folder>}" ;;
    publish) shift; publish "${1:?usage: ci/modules.sh publish <folder>}" ;;
    *) echo "usage: ci/modules.sh build <folder> | publish <folder>" >&2; exit 2 ;;
esac
