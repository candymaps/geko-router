# Packs de país del router

Todo lo propio de un país vive en su pack, `countries/<país>/`: las velocidades de sus vías, sus regiones, sus festivos, sus extractos de OSM y sus fuentes de tráfico. El motor es genérico y no se toca al añadir un país. Un pack es **datos, no código**.

| Pack | Fuentes | Particiones | Golden |
|---|---|---|---|
| `es` | OSM, sensores de Madrid y su histórico, incidencias de la DGT (desactivada) | Provincias, con cajas | Baleares, y Madrid con tráfico congelado |
| `lu` | OSM | El país entero | Luxemburgo entero |

## De dónde los lee el módulo

`geko` los baja firmados de `ghcr.io/candymaps/geko-router/countries/<país>` a `<datos>/router/packs/<país>/`, en `prepare`, antes de `plan`, `fetch`, `build` y `purge`, solo los de los países del ámbito. Un país sin pack es un error, como en el geocoder.

- `[packs] registry` de `geko-router.toml` cambia el registro.
- Una carpeta copiada a mano en `<datos>/router/packs/<país>/`, sin `.digest`, se usa tal cual y nunca se sustituye: `ci/packs.sh <datos>/router/packs`.
- `GEKO_ROUTER_PACKS=<carpeta>` lee los packs de esa carpeta y no descarga nada. Con `GEKO_ROUTER_PACKS=countries` se usan los del repositorio.
- Con `[data] offline`, solo vale la copia que haya en disco.

## Ficheros

```
countries/<país>/
├── pack.toml               # country, version, profiles, timezone y, si hace falta, [crs]
├── data/
│   ├── road-classes.toml   # velocidad por clase de vía y maxspeed_factor
│   ├── calendar.toml       # festivos: fixed (MM-DD) y easter_relative (días desde Pascua)
│   ├── regions.toml        # opcional: regiones que puede nombrar el ámbito, con su parent
│   └── region-boxes.tsv    # opcional: una caja por partición (código y cuatro números, con tabuladores)
├── sources/
│   ├── osm.toml            # opcional: extractos de Geofabrik, si el índice no basta
│   └── <fuente>.toml       # una fuente de tráfico, con su kind
├── golden/                 # rutas de referencia: no se publica
└── tests/pack.rs           # pruebas del pack: no se publica
```

Solo `pack.toml`, `data/` y `sources/*.toml` se publican. `version` se sube a mano en cada cambio de esos ficheros; es la etiqueta `pack-<n>`, y publicar otros ficheros bajo una versión ya publicada se rechaza.

### Regiones y extractos

Un ámbito solo puede nombrar regiones de `regions.toml`. Una región sin extracto propio en `sources/osm.toml` toma el de la región que la contiene, así que basta con listar las comunidades. Sin `osm.toml`, el extracto sale del índice de Geofabrik por el código ISO.

Un ámbito de regiones lee el extracto y se queda con lo que cae en sus cajas. Son rectángulos, así que el grafo incluye algo de las provincias vecinas, y una vía se corta en el borde de la caja. Una región sin caja, y sin hijas con caja, lee el extracto entero y lo avisa. Un ámbito de país entero no recorta nada.

### Fuentes

Toda fuente declara `kind`, `enabled`, `license = { name, attribution, url }`, `url`, `refresh_secs` (60 como mínimo), `ttl_secs` (al menos `refresh_secs`) y, si no cubre todo el país, `regions`: se usa si el ámbito contiene alguna de esas regiones. El resto del fichero es del proveedor de su `kind`.

`[sources.<nombre>]` en `geko-router.toml` cambia cualquier clave que exista en el fichero de la fuente, también en sus tablas. Una clave o una fuente que no existen son un error. Hoy solo se lee el `kind` `sensors-xml` (Madrid); el resto se declara, pero no tiene proveedor.

## Probar un pack

```bash
cargo test -p geko-module-router --test pack-lu
ci/golden.sh lu
```

Para añadir el crate de pruebas de un país nuevo, declara su `[[test]]` en `module/Cargo.toml`, apuntando a `../countries/<país>/tests/pack.rs`.

## Golden

`golden/sets.toml` nombra cada conjunto: sus extractos y, si lo tiene, una lectura congelada de un feed de tráfico. `golden/<conjunto>.jsonl` lleva una línea por ruta, con origen, destino, `duration_s`, `distance_m` y, con feed, `traffic_duration_s`. Las escritas a mano llevan además `expect_km`, un rango **estimado a mano**, no medido en un mapa: sirve para detectar un disparate, no para validar una ruta al metro.

`golden/snapshot` da el `sha256` de cada fichero. Los ficheros están en `$GEKO_GOLDEN_DATA` (por defecto `/var/lib/geko/golden/router`), y los que faltan se bajan del paquete privado `ghcr.io/candymaps/geko-router/golden`, con `oras login ghcr.io`. El job `golden` de `ci.yml` los lee con el token del workflow.

- `ci/golden.sh [país…]` compara y falla si algo cambia en más de 0,1 s o 0,1 m.
- `ci/golden.sh --write [país…]` escribe lo que dan ahora. Revisa el diff antes de hacer commit: un cambio sin explicación es un fallo.
- `ci/golden.sh --publish [país…]` sube los datos congelados. Hazlo antes de hacer commit de un `golden/snapshot` nuevo, o el CI no los encontrará.
- El ejemplo `golden` con `--generate <n>` añade rutas aleatorias, de semilla fija, hasta llegar a `n`.

## Publicar

`ci/packs.sh publish` firma cada pack con `GEKO_PACK_SIGNING_KEY` (la clave del resto de artefactos de geko) y sube los que han cambiado. Lo hace `.github/workflows/publish.yml` en cada push a `main`, y necesita el secreto `GEKO_PACK_SIGNING_KEY` en el repositorio, y `GEKO_HOST_TOKEN` si el repositorio del host es privado.

La firma va con su propio dominio (`geko-router-pack`), así que un pack del geocoder nunca pasa por uno del router. La primera vez, el paquete de cada país nace privado en ghcr.io: hay que hacerlo público en «Package settings», o nadie podrá bajarlo.

`GEKO_PACKS_DRY_RUN=1 ci/packs.sh publish` firma y se para antes del registro, y admite una clave de prueba.
