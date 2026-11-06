#!/usr/bin/env bash
# Measures the router against OSRM on the same extract, by hand (docs/ROUTER_PLAN.md, «Medición»):
#
#   ci/ab-osrm.sh <work dir> <geko binary> <dir of the module> <extract.osm.pbf> [rounds]
#
# <work dir> is the data directory of a geko with `geko.toml` in it and a bundle published, so that
# <work dir>/router/graph/graph.bin exists; <dir of the module> holds `router/module.toml` and the
# library (GEKO_MODULES). While the country packs are not published, GEKO_ROUTER_PACKS names their
# folder (`countries/` of this repository). The binary of the examples is taken from $CARGO_TARGET_DIR/release/examples.
#
# What makes the comparison fair, each learned the hard way:
# - geko, OSRM and wrk run in containers on the same network: a container with `--network host` is on
#   the virtual machine of Docker and cannot reach a process of WSL, and a published port goes through
#   a proxy of Docker that adds tens of milliseconds to some requests.
# - wrk keeps its connections open; a client that opens one per request measures the client.
# - The servers have cores 0-7 and the client 8-15, and they take turns in every round.
# - osrm-partition goes before osrm-contract: the other way round, CH has no metrics to load.
#
# MIN_LAT leaves out the pairs south of it, when OSRM has a smaller extract than the router: Spain is
# two extracts, and OSRM reads one, so MIN_LAT=30 leaves out the Canary Islands. Nothing else may run
# on the machine meanwhile: it refuses to start if a load is high, and a run much longer than the
# others (a machine that slept) is to be thrown away.
set -euo pipefail

work=$(realpath "$1")
geko=$(realpath "$2")
modules=$(realpath "$3")
extract=$(realpath "$4")
rounds=${5:-3}
pairs=${PAIRS:-20000}
seconds=${SECONDS_PER_RUN:-12}
image=${OSRM_IMAGE:-ghcr.io/project-osrm/osrm-backend:latest}
examples=${CARGO_TARGET_DIR:-target}/release/examples
graph=$work/router/graph/graph.bin
osrm=$work/osrm
wrk_dir=$work/wrk
port=18090

load=$(cut -d' ' -f1 /proc/loadavg)
awk "BEGIN { exit !($load < 3) }" || { echo "the machine is busy (load $load)" >&2; exit 1; }

printf 'FROM alpine:3\nRUN apk add --no-cache wrk\n' | docker build -q -t router-bench-wrk - > /dev/null
mkdir -p "$osrm" "$wrk_dir" "$work/bin"
cp "$geko" "$work/bin/geko"

osrm_run() { docker run --rm -v "$osrm:/data" "$image" "$@"; }
if [ ! -e "$osrm/extract.osrm.hsgr" ]; then
    cp -n "$extract" "$osrm/extract.osm.pbf"
    for step in "osrm-extract -p /opt/car.lua /data/extract.osm.pbf" "osrm-partition /data/extract.osrm" "osrm-customize /data/extract.osrm" "osrm-contract /data/extract.osrm"; do
        started=$(date +%s)
        # shellcheck disable=SC2086
        osrm_run $step 2>&1 | grep -E 'peak bytes|error' || true
        echo "${step%% *}: $(($(date +%s) - started)) s"
    done
fi

for kind in geko osrm; do
    for shape in numbers geometry; do
        extra=""
        [ "$shape" = geometry ] && extra=geometry
        # shellcheck disable=SC2086
        "$examples/urls" "$graph" "$kind" "$pairs" $extra > "$wrk_dir/$kind-$shape.txt"
    done
done
if [ -n "${MIN_LAT:-}" ]; then
    for shape in numbers geometry; do
        # The same lines of both lists, by the coordinates of those of geko.
        keep=$(sed -E 's/.*from=([-0-9.]+),[-0-9.]+&to=([-0-9.]+),.*/\1 \2/' "$wrk_dir/geko-$shape.txt" | awk -v m="$MIN_LAT" '$1 >= m && $2 >= m { print NR }')
        for kind in geko osrm; do
            awk 'NR == FNR { wanted[$1]; next } FNR in wanted' <(echo "$keep") "$wrk_dir/$kind-$shape.txt" > "$wrk_dir/$kind-$shape.kept"
            mv "$wrk_dir/$kind-$shape.kept" "$wrk_dir/$kind-$shape.txt"
        done
    done
fi
wc -l "$wrk_dir"/*.txt | tail -1

cat > "$wrk_dir/urls.lua" <<'LUA'
local urls = {}
local at = 0
function init(args)
  for line in io.lines(args[1]) do urls[#urls + 1] = line end
  at = math.random(#urls)
end
function request()
  at = at + 1
  if at > #urls then at = 1 end
  return wrk.format("GET", urls[at])
end
LUA

cleanup() { docker rm -f geko-srv osrm-ch osrm-mld > /dev/null 2>&1 || true; }
trap cleanup EXIT
cleanup
started=$(date +%s%N)
docker run -d --name geko-srv --network host --cpuset-cpus 0-7 -v "$work:$work" -v "$modules:$modules" \
    -e "GEKO_CONFIG=$work/geko.toml" -e "GEKO_DATA=$work" -e "GEKO_MODULES=$modules" debian:stable-slim "$work/bin/geko" serve > /dev/null
docker run -d --name osrm-ch --network host --cpuset-cpus 0-7 -v "$osrm:/data" "$image" osrm-routed --algorithm ch --threads 8 -p 5000 /data/extract.osrm > /dev/null
docker run -d --name osrm-mld --network host --cpuset-cpus 0-7 -v "$osrm:/data" "$image" osrm-routed --algorithm mld --threads 8 -p 5001 /data/extract.osrm > /dev/null

client() { docker run --rm --network host "$@"; }
reach() { client router-bench-wrk sh -c "wget -q -O- -T 3 '$1' 2>/dev/null | head -c 60"; }
for _ in $(seq 300); do
    reach "http://127.0.0.1:$port/health" | grep -q '"bundle":"' && break
    sleep 1
done
echo "geko answers after $((($(date +%s%N) - started) / 1000000)) ms"
reach "http://127.0.0.1:$port/health" | grep -q '"bundle":"' || { docker logs geko-srv 2>&1 | tail -3; exit 1; }
for osrm_port in 5000 5001; do
    for _ in $(seq 300); do
        reach "http://127.0.0.1:$osrm_port/route/v1/driving/-3.70,40.41;-3.69,40.42?overview=false" | grep -q '"Ok"' && break
        sleep 1
    done
    reach "http://127.0.0.1:$osrm_port/route/v1/driving/-3.70,40.41;-3.69,40.42?overview=false" | grep -q '"Ok"' || { echo "OSRM on $osrm_port does not answer" >&2; exit 1; }
done

memory() { docker exec geko-srv grep -E 'VmRSS|VmHWM' /proc/1/status; docker stats --no-stream --format '{{.Name}} {{.MemUsage}}' geko-srv osrm-ch osrm-mld; }
echo "== memory at rest"
memory

run() {
    echo "-- $1"
    client --cpuset-cpus 8-15 -v "$wrk_dir:/w" router-bench-wrk wrk -t4 -c16 "-d${seconds}s" --latency -s /w/urls.lua "http://127.0.0.1:$2" -- "/w/$3" 2>&1 |
        grep -E 'Requests/sec|Non-2xx|Socket errors|50%|90%|99%|requests in'
}
for shape in numbers geometry; do
    for round in $(seq "$rounds"); do
        echo "==== $shape, round $round"
        run geko "$port" "geko-$shape.txt"
        run osrm-ch 5000 "osrm-$shape.txt"
        run osrm-mld 5001 "osrm-$shape.txt"
    done
done

echo "== memory after the load"
memory
echo "== on disk"
du -sh "$work/router/graph"
du -ch "$osrm"/extract.osrm* | tail -1
