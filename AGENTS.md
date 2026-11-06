# geko-router

The routing module of [geko](https://github.com/candymaps/geko), a modular, multi-country geospatial engine: routes, matrices and the nearest street on the road network of the scope of `geko.toml`, with a graph that takes turn costs and restrictions and a Customizable Contraction Hierarchy, so that traffic changes the weights without stopping the service. Goals: speed, efficiency and quality, decided always with measurements. That it is faster than OSRM is a hypothesis until `ci/ab-osrm.sh` says so.

The plan, with what is decided, what is open, the steps and the measurements, is in [docs/ROUTER_PLAN.md](docs/ROUTER_PLAN.md). Read it before designing or implementing anything, and update it when a point is finished or a decision is made. The guide of the country packs is in [countries/README.md](countries/README.md), and the contract with the host in `docs/MODULES_PLAN.md` of the host.

## Environment

- Building and testing (cargo, `ci/test.sh`) happen in WSL Debian. The code lives in each machine's repository folder, which in WSL is `/mnt/<drive>/…`. The host sits beside this repository (`../geko`). From PowerShell, run each WSL command from a script: the quotes and `$` of an inline command get mangled on the way.
- Set `CARGO_TARGET_DIR` outside `/mnt/`, on the Linux disk, to build fast.
- The host is [candymaps/geko](https://github.com/candymaps/geko): the SDK (`geko-module-sdk`), `geko-fetch` and `geko-artifact` are git dependencies of it (`Cargo.toml`), at the tag `sdk-<contract>`. `ci/modules.sh` and `ci/packs.sh` get their signing tools at the commit `Cargo.lock` resolved for that tag.
- The private key that signs the module and the packs is in `~/.config/geko/pack-signing.key` (and in the `GEKO_PACK_SIGNING_KEY` GitHub secret); the public one, in `TRUSTED_KEYS` of `geko-artifact` in the host. Never print it or copy it into the repository.
- Nothing of geko loose in `~`: everything generated goes under `/var/lib/geko`, never in the repository. The frozen data of the golden are in `/var/lib/geko/golden/router`, and the work files of the measurements in `/var/lib/geko/router-bench`.
- Always measure performance in `--release`, with nothing else running on the machine and against OSRM in the same session (`ci/ab-osrm.sh`, which says what makes the comparison fair): a figure of another day or another machine is not comparable.
- Publishing: `publish.yml`, on every push to `main` (the merge of a pull request that passed the checks), publishes the country packs that changed (`ci/packs.sh`) and the module (`ci/modules.sh`, compiled on the Debian of the image of geko, to `ghcr.io/candymaps/geko-router`) if the push touches something that goes into it, with the version in `Cargo.toml` (`<version>-<commit>`). Raising the version in `[workspace.package]` is making a release: in addition, `<version>` on the module and the git tag `v<version>` on the pushed commit, with its GitHub release. The CI signs with the `GEKO_PACK_SIGNING_KEY` secret. A package is born private: make it public in GitHub the first time.
- A published version of a pack never changes: any change to its files, its signature included, needs a new `version` in its `pack.toml`.
- GitHub CI (`ci.yml`) checks every pull request: `ci/test.sh` and the golden if it changes what the checks build (`ci/affected.sh`), nothing if it only changes documentation, the CI or the publication. Its `test` and `golden` jobs and `protected.yml` (which fails a pull request by anyone but the owner that changes the files of the project and how strict the checks are) are the statuses that the protection of `main` and `dev` requires; `pr-comment.yml` says on the pull request whether the checks passed, failed or were not needed.
- Checks, the same locally and in CI (GitHub only calls the script): `ci/test.sh [--variant code,contracts,golden]`. `code`: clippy, dependency rules (the module speaks to the host only through the SDK) and cargo-deny; `contracts`: tests, those of each pack and the conformance suite of the module contract. `golden`, not in the default variants but its own job of the CI: `ci/golden.sh`, the 300 routes of the packs on frozen data, pulled from the private package `ghcr.io/candymaps/geko-router/golden`. Its numbers detect changes, not correctness: only the routes written by hand carry a range.

## Branches and commits (Gitflow)

- **Nothing is ever pushed directly to `main`**: no commits, no pushes.
- All new development starts from `dev`, in its own branch with a Gitflow name: `feature/<name>`, `bugfix/<name>`… It ends with a PR from that branch to `dev`.
- A release (a push to `main`) is always a PR from `dev` to `main`.
- All commits follow [Conventional Commits](https://www.conventionalcommits.org/): `<type>(<optional scope>): <description>`, with type `feat`, `fix`, `perf`, `refactor`, `docs`, `test`, `build`, `ci` or `chore`. A breaking change takes `!` after the type (`feat!: …`) or a `BREAKING CHANGE:` footer. The title of each PR follows the same format.

## Key rules

- geko is a host and its modules: this repository is the `router` module, enabled by `[modules]` of `geko.toml`. The host knows it only through the contract of `geko-module-sdk` (`Module`), and the module uses nothing of the host but the SDK and its artifacts: `ci/test.sh` checks it. It serves its routes under `/router/`, reads its own `geko-router.toml` and builds its own part of the bundle. Every option of the module is documented in its README, not in `geko.toml` or the README of the host. A disabled module does not download, does not instantiate, does not open files and does not register routes.
- A country is data, not code: what changes between countries lives in its pack (`countries/<country>/`), read at run time, and the engine knows no concrete source. A new source of a known kind does not touch the engine; a new kind is a new provider. A country without a pack is an error.
- Every download goes through the `Downloader` of the host that the SDK passes. Downloading requires `accept_licenses = true`. No download protected by a CAPTCHA or other access control is automated.
- Every source declares its license in its pack, and it reaches the bundle manifest and `/v1/attribution`. A source without a clear license does not go in the pack, or goes disabled, as the DGT does (`docs/ROUTER_PLAN.md`, «Licencias»). Read the conditions of a source before publishing it, not after.
- Traffic is off by default, and it degrades: a source that is down or out of date falls back, it never breaks a route. Every response says how much of the route had data (`traffic.coverage`), so that «no queues» is never confused with «no data».
- Live traffic is never in the bundle, which is immutable: it is read while serving, lives in memory, and new weights are published by customizing the CCH and swapping them, with no restart.
- Build incremental by input hash (PROJECT.md §8 of the host): each stage (fragment of an extract, graph, hierarchy) leaves a stamp and is skipped if neither its code nor its inputs change; the options that change what is generated are in the stamps. `geko build --force` redoes everything. The build must be deterministic.
- A graph is one for the whole scope, because a route crosses borders: `[[scope]]` says what is served.
- Selective abstraction: only the ports of the SDK are traits. Everything else is plain code; its trait is extracted when a second real implementation appears.
- Licenses: the code is AGPL-3.0-only (`LICENSE`) and contributions are accepted under that same license, with no CLA: only the collaborators of the repository may open pull requests. The graph is a derived database of OSM (ODbL).

## Optimizations: what worked and what did not

Before optimizing, check this list so as not to repeat what was already discarded. When you try something, add it here with its measurement, whether it works or not. The full measurements are in `docs/ROUTER_PLAN.md`, «Seguimiento».

**Worked:**
- Leaving the ferries out of the street index (2026-11-04, Baleares): they filled thousands of cells; from 690 to 98 MB, and from 4.7 to 0.47 s of load.
- `query_many`, one search for all the combinations of directions instead of up to 4 (2026-11-04, Baleares, alternate pairs): 1.5 to 1.9 times the requests per second without geometry, and 1.0 to 1.4 with it.
- `Snapper` in the order of the graph and in 16 bands of rows, each in its thread (2026-11-04, Spain): 6.8 to 2.4 s. It also made the index deterministic: 118 of 400 routes of Baleares changed between two starts, and now none.
- `table` with labels on the elimination tree (2026-11-04, Baleares, 1,000×1,000): 0.25–0.44 s, against about 230 s for one query per cell.
- Customizing with threads (2026-11-04, Spain): 12 s with one, 4.2 s with 16.
- One stage per extract, with its stamp (2026-11-04): with nothing changed, Baleares and Luxembourg take 2 ms; with new speeds in `lu`, only Luxembourg is read again (4.3 s, against 24 s with the download).

**Did not work (do not repeat):**
- `mmap` for the graph and the hierarchy (2026-11-04, Spain): reading is 10 % of the load (about 2 s of 20); what weighs is the computing. It remains an idea only to share the page cache between processes.
- An R-tree of Hilbert instead of the grid (2026-11-04, Baleares): snapping takes 13 µs of a 230 µs route, less than 10 % of a request.
- A fast hasher alone in `Snapper` (2026-11-04, Spain): 7.1 s, no better.
- A stamp per province (2026-11-04): the hierarchy is global, and a new PBF of Spain changes every province at once. Stages go by extract.

**Not measured on its own:** mimalloc in the library of the module.
