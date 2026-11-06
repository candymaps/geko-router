<p align="center">
  <img height="100" src="https://placehold.co/170x30/transparent/A4CC47?text=Geko+Router&font=raleway" alt="geko-router">
</p>
<h3 align="center">The routing module of geko.</h3>
<p align="center">Routes on the roads of your scope, served by geko alone.</p>
<p align="center">
  <a href="https://github.com/candymaps/geko-router/releases/latest"><img src="https://img.shields.io/github/v/release/candymaps/geko-router?label=version&color=blue&style=plastic" alt="Version"></a>
  <a href="https://www.gnu.org/licenses/agpl-3.0"><img src="https://img.shields.io/badge/license-AGPL--3.0-orange?style=plastic" alt="License"></a>
  <a href="https://github.com/orgs/candymaps/packages?repo_name=geko-router"><img src="https://img.shields.io/badge/module-ghcr.io-informational?style=plastic&logo=docker" alt="Module"></a>
  <a href="https://github.com/candymaps/geko-router/actions/workflows/publish.yml"><img src="https://github.com/candymaps/geko-router/actions/workflows/publish.yml/badge.svg?branch=main" alt="publish"></a>
</p>

# geko-router

The routing module of [geko](https://github.com/candymaps/geko). **Status: published, and off by default in geko.** It meets the contract of the host and passes its conformance suite; it downloads the OSM extracts of the scope, builds a graph with turn costs and restrictions and a Customizable Contraction Hierarchy, leaves its part of the bundle (all of Spain in about 4 minutes, 1.1 GB on disk) and serves it. Cars only, for now. The plan, with its decisions and measurements, is in [docs/ROUTER_PLAN.md](docs/ROUTER_PLAN.md) (in Spanish).

What it serves, under `/router/`:

- **Routes** (`/router/v1/route`): the fastest way between two points, with its time, length and line, its steps (`steps=true`) and up to three alternatives (`alternatives=n`). `traffic=none|historical|live` and `depart_at` (RFC 3339) say what the weights are, here and in matrices.
- **Matrices** (`/router/v1/table`): times and distances from many points to many, up to 1,000,000 cells and 2,000 points a side, by `GET` or by `POST`. A matrix of 1,000×1,000 takes less than half a second in Baleares.
- **The nearest street** (`/router/v1/nearest`).
- **Isochrones** (`/router/v1/isochrone`): what can be reached from a point in up to four times, as GeoJSON polygons.
- **Map matching** (`/router/v1/match`): the roads a trace of up to 2,000 points went along.
- **Trips** (`/router/v1/trip`): a good order to visit up to 100 points, and its routes. It is a heuristic, not always the best order.
- **Status** (`/router/v1/status`), the **API** (`/router/openapi.yaml`, also listed in `/docs` of geko) and a **demo** (`/router/`) of routes, isochrones, trips and map matching: click on the map.

On top of the same foundation as the rest of geko:

- **Fully automatic:** it downloads what the scope of `geko.toml` needs, sharing the extracts with the other modules, builds the graph and keeps it up to date.
- **Measured against OSRM** on all of Spain, on the same machine: with the line of the route, 1.5 times the requests per second of OSRM CH; without it, 90 % of those of CH and more than MLD. It takes 1.1 GB on disk against 4.1 GB, and 4.5 GiB of memory under load against 3.8 GiB.
- **Any combination of countries and regions**, in a single graph, because a route crosses borders. A scope of provinces cuts the graph to their boxes.
- **A new country is a data pack**, with no changes to the engine: speeds, regions, holidays and traffic sources live in [countries/](countries/README.md). There are packs for Spain (`es`) and Luxembourg (`lu`), published and signed.
- **Traffic, as an option:** the live sensors of Madrid and the incidents of the DGT change the weights while it serves, with no restart, and a closed road is closed. The usual speed of each hour of the week is learned when the bundle is built, and `depart_at` asks for it. Every response says how much of the route had data (`traffic.coverage`). The speeds are estimates: they come from levels of service, measured on fast roads, and the delay of city junctions is not in them.

## Installation and usage

It is enabled like any module of geko, in `geko.toml`:

```toml
[modules]
router = true
```

Its own options go in `geko-router.toml`, beside `geko.toml`; every one is optional, and an unknown one is an error that names the file and the key:

```toml
[osm]
mirror = "auto"             # Geofabrik, or the URL of a mirror with its layout (the extracts are shared with the other modules)

[packs]
registry = "ghcr.io/candymaps/geko-router/countries"    # where the country packs are, without the country

[profiles]
default = "car"             # the profile of the graph: car, truck, bike or foot. One for each bundle: for several, an instance of geko for each

[traffic]
enabled = false             # true: match the sensors and learn the usual speeds in the build (about 200 MB of downloads in Madrid)
live = true                 # with enabled, read the sources live while serving

[sources.madrid-sensors]    # any field of countries/es/sources/madrid-sensors.toml, by the name of that file
enabled = true              # also its cadence (refresh_secs) and how long a reading lasts (ttl_secs)

[serve]
preload = true              # false: load the graph with the first request that needs it, which waits for it;
                            # until then v1/status answers 503, and the process takes no memory for it
```

`[sources.<name>]` admits only the keys of the file of that source, and naming a source that no pack of the scope has is an error. The profile and the speeds of the pack change what is generated, so they are in the stamps of the build.

Downloading requires `[data] accept_licenses = true` in `geko.toml`, as for every module. A country without a pack is an error. Packs are downloaded, signed, to `<data>/router/packs/<country>/`.

## License

geko-router is licensed under the [GNU Affero General Public License v3.0](LICENSE) (AGPL-3.0-only). You may use, study, modify and share it freely. If you distribute it, modified or not, you must pass on its source code under the same license; if you modify it and let others use it over a network, you must offer them the source code of your version. Running it, or modifying it for internal use only, requires nothing more.

The graph is a derived database of OpenStreetMap (ODbL) and carries its attribution. With traffic on, `/v1/attribution` also gives each source that is used, its license and that its data are modified, and `traffic.source_updated` the date of the feed. Sources are not redistributed: each installation downloads them from their origin. The source of the DGT is disabled by default: `[sources.dgt-incidents] enabled = true` is your decision, and it takes about 1.5 GB a day. With traffic on, the bundle says the license of each part (`part_licenses`). Contributions are accepted under the same license, from the collaborators of the repository; see [CONTRIBUTING.md](CONTRIBUTING.md).

Read the full terms in [LICENSE](LICENSE).

## Technologies

| Technology | What it is used for |
|---|---|
| **Rust** (2024 edition) | The module: its build stages and the service of the routes |
| `geko-module-sdk` | The contract with the host, at the tag `sdk-<contract>` of [geko](https://github.com/candymaps/geko) |
| Customizable Contraction Hierarchies | Routes and matrices, with weights that change without contracting again |
| [osmpbf](https://github.com/b-r-u/osmpbf) | Reading the OSM extracts |
| [mimalloc](https://github.com/microsoft/mimalloc) | The allocator of the library of the module |
