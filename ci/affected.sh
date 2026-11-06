#!/usr/bin/env bash
# Whether what changed since a commit can fail the checks of ci/test.sh, for the CI of pull requests:
#
#   ci/affected.sh <base> [<head>]
#
# prints the arguments for ci/test.sh (`--variant code,contracts`) for what changed from <base> to
# <head> (without it, to the working tree), or nothing when no check covers it (only documentation
# and what the checks do not build: the CI and the publication).
set -euo pipefail
cd "$(dirname "$0")/.."

base=${1:?usage: ci/affected.sh <base> [<head>]}
range=("$base" ${2:+"$2"})

while IFS= read -r file; do
    case $file in
        *.md | LICENSE | .github/* | ci/ab-osrm.sh | ci/affected.sh | ci/modules.sh | ci/oras.sh | ci/packs.sh) ;;
        *) echo "--variant code,contracts"; exit 0 ;;
    esac
done < <(git diff --name-only "${range[@]}")
