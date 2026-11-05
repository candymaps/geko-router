#!/usr/bin/env bash
# The checks of every change of the router module, the same locally and on the CI (GitHub Actions):
# the CI files only prepare Rust and the cache, then run this script.
#
#   ci/test.sh [--variant <variants>]
#
# --variant, what is checked, comma-separated (default: code,contracts):
#   code          clippy (warnings are errors), dependency rules (the module speaks to the host
#                 only through the contract of the SDK), and licenses and security advisories
#                 (cargo-deny)
#   contracts     unit tests, the tests of each country pack and the conformance suite of the module
#                 contract
#   golden        the golden routes of the packs (ci/golden.sh), on frozen data pulled from the
#                 registry. Not by default: on the CI it is a job of its own
set -euo pipefail
cd "$(dirname "$0")/.."

lint() {
    rustup component list --installed 2>/dev/null | grep -q '^clippy' || rustup component add clippy
    cargo clippy --workspace --all-targets --locked -- -D warnings
}

test() {
    cargo test --workspace --locked
}

direct_dependencies() { cargo tree -p "$1" -e normal --depth 1 --prefix none --locked | tail -n +2 | cut -d' ' -f1 | sort -u; }

# Fails, naming them, if `found` (one per line) has any name not in `allowed` (space-separated).
only() {
    local what=$1 allowed=$2 found=$3 name bad=""
    for name in $found; do
        [[ " $allowed " == *" $name "* ]] || bad="$bad $name"
    done
    [ -z "$bad" ] || { echo "$what:$bad" >&2; return 1; }
}

# What the dependency graph can show of the rules of the host (its AGENTS.md): the module knows the
# host only through the contract of the SDK, and downloads only with its Downloader (geko-fetch) and
# geko-artifact: no HTTP client of its own. A new dependency is either a rule broken or a decision to
# write down here.
architecture() {
    local failed=0
    only "geko-module-router depends on" "geko-artifact geko-fetch geko-module-sdk osmpbf serde serde_json toml tracing" "$(direct_dependencies geko-module-router)" || failed=1
    only "geko-module-router-dylib depends on" "geko-module-router geko-module-sdk mimalloc tracing-subscriber" "$(direct_dependencies geko-module-router-dylib)" || failed=1
    return $failed
}

CARGO_DENY_VERSION=0.20.2

# Licenses of the dependencies (deny.toml) and security advisories of the versions in Cargo.lock.
deny() {
    local bin
    bin=$(command -v cargo-deny || true)
    if [ -z "$bin" ] || ! "$bin" --version | grep -q " $CARGO_DENY_VERSION\$"; then
        local dir="${CARGO_HOME:-$HOME/.cargo}/bin" name="cargo-deny-$CARGO_DENY_VERSION-x86_64-unknown-linux-musl"
        mkdir -p "$dir"
        curl -fsSL "https://github.com/EmbarkStudios/cargo-deny/releases/download/$CARGO_DENY_VERSION/$name.tar.gz" \
            | tar -xzf - -C "$dir" --strip-components=1 "$name/cargo-deny"
        bin="$dir/cargo-deny"
    fi
    "$bin" --locked check
}

ALL_VARIANTS=code,contracts,golden
VARIANTS=code,contracts

usage() { echo "usage: $0 [--variant $ALL_VARIANTS]" >&2; }

# Whether the variant `$1` was asked for.
wants() { [[ ",$VARIANTS," == *",$1,"* ]]; }

while [ $# -gt 0 ]; do
    case $1 in
        --variant) VARIANTS=${2:?--variant takes a list}; shift 2 ;;
        --variant=*) VARIANTS=${1#*=}; shift ;;
        -h | --help) usage; exit 0 ;;
        *) usage; exit 2 ;;
    esac
done
for variant in ${VARIANTS//,/ }; do
    [[ ",$ALL_VARIANTS," == *",$variant,"* ]] || { echo "unknown variant $variant" >&2; usage; exit 2; }
done

# Runs a check in a subshell, where errexit stops it at its first failure as when it ran alone,
# and goes on with the next one: the summary names the checks that failed.
FAILED=()
run() {
    local name=$1 status
    shift
    echo "==> $name"
    set +e
    (
        set -e
        "$@"
    )
    status=$?
    set -e
    [ "$status" = 0 ] || FAILED+=("$name")
}

if wants code; then
    run lint lint
    run architecture architecture
    run deny deny
fi
if wants contracts; then
    run test test
fi
if wants golden; then
    run golden ci/golden.sh
fi
if [ ${#FAILED[@]} -gt 0 ]; then
    echo "failed: ${FAILED[*]}" >&2
    exit 1
fi
echo "passed"
