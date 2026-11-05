# Módulo `geko-router`: plan

Estado (2026-11-05, bloques B y C hechos, con los packs publicados; medición del punto 4 aplazada): en desarrollo, en la rama `feature/router-module` de `candymaps/geko-router`, sin publicar. Lo hecho y lo que falta está en «Seguimiento». Las decisiones abiertas están al final.

## Objetivo

Un módulo de enrutamiento de `geko`, en Rust, que mejore a OSRM con los mismos objetivos del proyecto: **velocidad, eficiencia y calidad**. Vive en su propio repositorio (`candymaps/geko-router`), se publica como artefacto firmado y se carga como `DylibModule`, según [MODULES_PLAN.md](https://github.com/candymaps/geko/blob/dev/docs/MODULES_PLAN.md) del host. El host no se toca: rutas bajo `/router/`, configuración en `geko-router.toml`, parte propia del bundle.

Que sea más rápido que OSRM es una **hipótesis**, no un dato. Se da por cierta solo con la medición A/B de la fase 1 (sección «Medición»).

## Reparto

| | Host (`candymaps/geko`) | Módulo (`candymaps/geko-router`) |
|---|---|---|
| Código | Lo de siempre: descarga y carga del módulo, bundles, HTTP común, atribución | Lectura del OSM, grafo, índice, consultas, `/router/v1/*`, caché, órdenes de depuración |
| Datos | Ninguno | Packs de país (`countries/<país>/`), incluidos los de tráfico |
| Contrato | `geko-module-sdk` (sin cambios previstos) | `describe`, `plan`, `fetch`, `build`, `load`, `handle`, `metrics`… |

Regla de siempre: lo que solo tiene sentido para el router (grafo, perfiles, proveedores de tráfico) vive en el módulo. Cualquier cambio que pida tocar el contrato se trata como cambio de contrato (AGENTS.md del host).

## Fuentes de datos

- **Red viaria:** OpenStreetMap, el mismo extracto que ya baja el geocoder (Geofabrik, por ámbito). El módulo declara sus propias descargas con el `Downloader` del host; la caché de descargas evita bajar dos veces el mismo fichero.
- **Tráfico:** en **packs de país**, igual que CartoCiudad en el geocoder (sección siguiente).

## Datos de tráfico en packs por país

Los datos de tráfico dependen del país: cada administración publica en un formato, con una cobertura y una licencia distintas. Por eso, como las fuentes de CartoCiudad, **van en el pack del país** y el motor no sabe de ninguna fuente concreta.

```
countries/es/
├── pack.toml             # país, versión, perfiles, zona horaria
├── data/
│   ├── road-classes.toml # velocidades por clase de vía
│   ├── calendar.toml     # festivos nacionales (para los perfiles horarios)
│   ├── regions.toml      # regiones que puede nombrar el ámbito
│   └── region-boxes.tsv  # una caja por partición, para recortar el grafo
├── sources/
│   ├── osm.toml          # extractos de OSM del país (igual que el del geocoder)
│   ├── madrid-sensors.toml   # tráfico en vivo: sensores de Madrid
│   └── dgt-incidents.toml    # incidencias y cortes de la DGT
├── golden/               # rutas de referencia con y sin tráfico
└── tests/
```

La guía de los packs (ficheros, de dónde se leen, golden y publicación) está en [countries/README.md](../countries/README.md).

Como en el geocoder, el pack es **datos, no código**: lo que cambia entre países es una declaración TOML (`kind`, URL, licencia, formato, frecuencia, cómo se casa el dato con el grafo). El código genérico vive en proveedores por `kind` (`datex2`, `csv-sensors`, `json-sensors`, `ogc-api`…). Añadir un país o una fuente nueva de un tipo ya conocido **no toca el motor**. Un tipo de fuente nuevo es un proveedor nuevo, y no se extrae un trait hasta que haya una segunda implementación real (regla de abstracción selectiva); los dos primeros tipos, sensores y DATEX II, ya lo son.

Cada declaración de fuente lleva, como la de CartoCiudad, `license = { name, attribution, url }`, que entra en el manifiesto del bundle y en `/v1/attribution`. Cada una se puede anular o desactivar desde `geko-router.toml` (`[sources.<nombre>] enabled = false`). Se publican firmados en `ghcr.io/candymaps/geko-router/countries/<país>` con el mecanismo genérico de `geko-artifact`, con etiqueta `pack-<versión>` y la misma regla de inmutabilidad que los del geocoder. La firma lleva su propio dominio (`geko-router-pack`): un pack del geocoder no pasa por uno del router, aunque compartan la clave.

### Tres capas de tráfico

Cada capa es opcional y degrada a la anterior si falta:

1. **Perfil histórico** (fase de generación): velocidad típica por arista, hora y día de la semana, aprendida de las mediciones de los sensores abiertos del país y ligada al calendario del pack. Cubre lo que se haya medido alguna vez; el resto usa la velocidad libre del perfil. Es la capa más barata y la que más aporta por sí sola.
2. **Sensores en vivo** (servicio): cada punto de medida se casa con una o varias aristas del grafo (map matching por proximidad, sentido y clase de vía, hecho una vez al generar) y su dato modifica el peso. Un dato que caduca (TTL por fuente) decae hacia el perfil histórico.
3. **Incidencias y cortes** (servicio): penalizan o cierran aristas, con su vigencia.

Los pesos nuevos se publican por **recustomización** (ver «Algoritmo») y se intercambian con `arc-swap`, sin parar el servicio y sin reiniciar. El refresco corre en una tarea de fondo con su propio calendario por fuente.

### Honradez de la cobertura

No existe un servicio gratuito y libre que dé velocidad por tramo en todas las carreteras de un país. Cada respuesta indica qué parte de la ruta tuvo dato en vivo, histórico o solo velocidad libre (`traffic.coverage`), para que el cliente no confunda «sin retenciones» con «sin datos».

### Fuentes candidatas (por verificar antes de escribir su pack)

Hallazgos de la investigación del 2026-11-03; **todo lo marcado «sin confirmar» debe comprobarse en la ficha de la fuente antes de comprometerse**.

| País | Fuente | Qué da | Licencia | Estado |
|---|---|---|---|---|
| ES | Ayuntamiento de Madrid, datos abiertos | Más de 4.000 puntos de medida, cada 5 min, intensidad y nivel de servicio (0 fluido, 1 lento, 2 retenciones, 3 congestión); no velocidad directa | CC BY 4.0 en el recurso de intensidad | Revisar cada conjunto y el cálculo del nivel (umbrales por punto) |
| ES | DGT, Punto de Acceso Nacional (`nap.dgt.es`) | Paneles y cámaras en DATEX II v3.6/3.7 (1 min); incidencias | Sin confirmar (aparecen licencias Creative Commons, sin variante) | Varios conjuntos marcados obsoletos; no se halló velocidad ni flujo por tramo |
| LU | cita.lu | Eventos en DATEX II (Alert-C) | Sin confirmar | No se halló flujo ni velocidad |
| NL | NDW | Flujo, velocidad, tiempos de viaje, colas | Sin confirmar | Candidato si se añade el país |
| FI | Digitraffic | JSON y DATEX II | Sin confirmar | Candidato |
| EE, LT | Tarktee; servicio de intensidad | Flujo en vivo | Registro gratuito (EE) | Candidatos |

Las fuentes comerciales (Google, TomTom, HERE) y de socio (Waze) no son de uso libre y quedan fuera.

### Licencias (revisión del 2026-11-03)

Revisión de lectura, no asesoramiento jurídico. geko no aloja estos datos: cada instalación los descarga de la fuente, con `accept_licenses = true`, como CartoCiudad.

**Madrid (sensores): usable.**
- La ficha del conjunto `202087-0-trafico-intensidad` dice «Creative Commons Attribution 4.0 International (CC BY 4.0)».
- CC BY 4.0 permite uso comercial y obras derivadas con atribución. La atribución va a `/v1/attribution`: «Ayuntamiento de Madrid, datos abiertos (CC BY 4.0)».
- Leídas el 2026-11-05 (resumidas por la herramienta web, no el texto íntegro): [condiciones de uso](https://datos.madrid.es/pages/condiciones-de-uso), [condiciones generales](https://datos.madrid.es/pages/condiciones-generales-ayuntamiento-de-madrid) y [aviso legal](https://datos.madrid.es/pages/aviso-legal). El conjunto de histórico también es CC BY 4.0. Permiten el uso comercial, y copiar, modificar y combinar los datos. Ninguna limita el acceso automático.
- Obligan a citar la fuente, indicar la fecha de actualización si la hay, no desnaturalizar la información, no insinuar que el Ayuntamiento apoya el uso, conservar los metadatos y no reidentificar personas. El uso corre por cuenta de quien reutiliza. Las condiciones generales no mencionan CC BY: se cumplen las dos.
- Cómo se cumple, con el tráfico activo: `/v1/attribution` da la fuente, la licencia con su enlace y que los datos están modificados, y `traffic.source_updated` da la fecha del feed tal cual. Queda sin cerrar «no desnaturalizar»: los factores de nivel de servicio son provisionales hasta el punto 20, y antes de eso es mejor no presentar el tráfico como una medida fiable.
- Cuidado con la mezcla de licencias, igual que con CartoCiudad (CC BY) junto a OSM (ODbL): los pesos de tráfico y el grafo de OSM viajan en el mismo bundle. Conviene fijar qué parte del bundle es obra derivada de OSM (ODbL) y cuál lleva CC BY, y declararlo en el manifiesto.

**DGT (incidencias): usable con reservas.**
- La ficha del NAP dice «Creative Commons Attribution» (enlaza a la Open Definition, sin versión) y «Licence and Free of charge». Sin autenticación.
- Pero las condiciones de uso que enlaza esa ficha, el aviso legal de la DGT, dicen que los contenidos son titularidad de la DGT y tratan como infracción «la reproducción, distribución, comercialización o transformación no autorizadas», salvo uso personal y privado. No menciona reutilización, el Real Decreto 1495/2011 ni Creative Commons. Las dos cosas no encajan.
- Mientras la DGT no lo aclare por escrito, la fuente queda **desactivada por defecto** en el pack (`enabled = false`). Pedir a la DGT (contacto del NAP) que confirme: licencia y versión, uso comercial, obras derivadas, y la atribución exacta.
- Cobertura: red estatal, salvo País Vasco y Cataluña, que publican las suyas (en el NAP: «Incidencias DT-GV» e «Incidencias SCT»). Fuera de la red estatal, sin dato.
- No trae velocidad ni flujo: solo situaciones (cortes de calzada, de carril, obras, retenciones `slowTraffic`).

### Fuentes verificadas en el pack de España

Comprobadas con una petición real el 2026-11-03:

| Fuente | URL | Tamaño | Cabeceras | Cadencia |
|---|---|---|---|---|
| Madrid, sensores | `https://informo.madrid.es/informo/tmadrid/pm.xml` | 2,2 MB | `ETag`, `Last-Modified` | ~5 min |
| DGT, incidencias v3.7 | `https://nap.dgt.es/datex2/v3/dgt/SituationPublication/datex2_v37.xml` | 7 MB (220 KB con gzip) | `Cache-Control: max-age=39`, sin `ETag` | 1 min |

La DGT pide `Accept-Encoding: gzip`, o serían unos 10 GB al día. Madrid publica las coordenadas en ETRS89 UTM 30N (EPSG:25830) con coma decimal.

## Algoritmo

- **CCH (Customizable Contraction Hierarchies).** La topología (orden de nodos, contracción) se calcula una vez al generar; los pesos se aplican en una fase de **customización** que es rápida y se repite cuando cambia el tráfico. Frente a OSRM: su CH obliga a recontraer con cada cambio de pesos, y su MLD admite pesos nuevos pero consulta más lento; su tráfico es experimental y pide recargar el servidor.
- Referencia pública (reimplementación de terceros, no medición propia): recustomización completa en el orden de decenas de ms tras un cambio de arista, con consulta como punto flojo conocido. Esto se **mide con el grafo real** (España), no se asume.
- **Grafo:** con costes por giro y restricciones de giro de OSM desde el principio (modelarlo después obliga a rehacer el grafo). Perfiles en TOML (coche primero; bici, a pie y camión después), sin scripts.
- **Memoria:** el grafo y la jerarquía se leen enteros. El `mmap` se descartó por medición (punto 2): la lectura es el 10 % de la carga. Un ámbito de provincias recorta el grafo a sus cajas (punto 17).
- **Asignador:** mimalloc declarado en la `.so` del módulo (ver MODULES_PLAN.md, «Asignador»).

## API (`/router/v1/…`)

Fase 1: `route` (con alternativas y pasos), `nearest`, `table` (matriz muchos-a-muchos). Después: `match` (map matching), `isochrone`, `trip`. Todas bajo `/router/`, con su `openapi.yaml` y su demo en `/router/`, como el geocoder.

Parámetros de tráfico: `traffic=none|historical|live` y `depart_at` (RFC 3339) para ruta futura con perfil histórico. Por defecto, `live` si hay dato; la respuesta lleva `traffic.coverage`.

Formato: propio y documentado en `openapi.yaml`, sin capa de compatibilidad con OSRM (punto 8).

## Configuración (`geko-router.toml`)

```toml
[osm]
mirror = "auto"             # Geofabrik, o la URL de un espejo con su estructura

[packs]
registry = "ghcr.io/candymaps/geko-router/countries"

[profiles]
default = "car"             # el único que hay; entra en los sellos

[traffic]
enabled = false             # desactivado por defecto
live = true                 # con enabled, leer las fuentes en vivo

[sources.madrid-sensors]    # cualquier clave de countries/es/sources/madrid-sensors.toml
enabled = true

[serve]
preload = true              # false: cargar el grafo con la primera petición que lo necesite
```

Sustituye a `[traffic.madrid]`, `interval_secs` y `max_age_secs`: la cadencia y la caducidad son `refresh_secs` y `ttl_secs` de cada fuente, en su pack, y se pueden cambiar en `[sources.<nombre>]`.

- `[sources.<nombre>]` solo admite claves que existan en el fichero de la fuente, y nombrar una fuente que ningún pack del ámbito tiene es un error. Se comprueba con los packs a mano, en `prepare`.
- `[serve] countries`, que estaba en este plan, **se descartó** (2026-11-04): hay un único grafo para todo el ámbito, porque una ruta cruza fronteras, y `[[scope]]` ya dice qué países se sirven.
- Con `preload = false`, las rutas de `v1/route`, `v1/nearest` y `v1/table` disparan la carga y esperan a que acabe. `v1/status` responde 503 hasta entonces, porque no corre en el pool y no debe bloquearse.

Las opciones que cambian lo generado (perfil, velocidades del pack, recorte) entran en los sellos de las etapas. Una opción desconocida es un error que nombra el fichero y la clave, como en el geocoder.

## Bundle y generación

Etapas con sellos de entrada (hash de código y de entradas, como PROJECT.md §8), deterministas:

1. **Fragmento de cada extracto** (`router/fragments/<extracto>/`): sus vías, restricciones y nodos, con el perfil de su país y recortado a las regiones del ámbito. No va al bundle.
2. **Grafo** (`router/graph/`): la unión de los fragmentos, con giros y restricciones. Su sello depende del contenido de los fragmentos.
3. **Jerarquía** (`router/hierarchy/`): orden de nodos y contracción (CCH). Es lo caro, y su sello depende del contenido del grafo, así que un grafo que sale igual no la repite.

Las pendientes, con el bloque D: casado de los puntos de medida con aristas (punto 22) y perfiles históricos (punto 24).

El índice espacial se construye al cargar, con una rejilla: el R-tree de Hilbert se descartó por medición (punto 11).

El tráfico en vivo **no** va en el bundle, que es inmutable: se lee en servicio y vive en memoria.

## Medición

Antes de afirmar «más rápido que OSRM»:

- Mismo extracto OSM, misma máquina, consultas idénticas, 16 peticiones concurrentes, turnos alternos (regla de AGENTS.md: nunca comparar con una medición de otro momento).
- Rutas cortas, medias y largas; `table` de 10×10 a 1.000×1.000; arranque en frío y en caliente; memoria residente; tamaño del bundle; tiempo de generación.
- Con tráfico: tiempo de recustomización y consulta durante la customización.
- Calidad: mismos pares origen-destino contra OSRM y contra una referencia (diferencia de duración y distancia, rutas distintas). La velocidad sin calidad no cuenta.
- Cada intento, funcione o no, se anota en «Optimizaciones» del repositorio del módulo.

## Pasos

Cada paso deja resultados verificables antes del siguiente.

1. **Esqueleto del módulo.** Repositorio, `describe`, `plan`/`fetch` del OSM, `handle` que responde 503, conformidad de la SDK en verde. Publicación firmada.
2. **Grafo y perfil de coche.** Grafo con giros y restricciones, `nearest`, golden de Baleares (`ES-PM`) y Luxemburgo.
3. **CCH y `route`.** Orden, contracción, customización, consulta. Primera medición A/B contra OSRM con Baleares, luego España completa.
4. **`table`** y la segunda medición.
5. **Packs de país**: formato `pack.toml`/`sources/`, publicación firmada, packs `es` y `lu` solo con OSM y perfil (sin tráfico aún).
6. **Tráfico: histórico y en vivo.** Proveedores por `kind` (`csv-sensors`/`json-sensors`), casado de sensores, recustomización en caliente, `traffic.coverage`. Primer pack con tráfico real: la fuente de Madrid.
7. **Incidencias** (DATEX II): cortes y penalizaciones.
8. **`match`, isócronas, bici, a pie, camión.**
9. **Documentación** en cada paso: este plan, PROJECT.md (módulos, §14 hoja de ruta y §16 pendientes), README del host (módulo nuevo) y del módulo.

## Seguimiento

Lista de trabajo, en el orden previsto. Al cerrar un punto se marca aquí, con su commit o su medición. Los marcados con **[decisión]** o **[gestión]** dependen del propietario del proyecto.

**Hecho (2026-11-04):** grafo con giros, restricciones y ferris; CCH con customización en caliente; `v1/route` con pasos y alternativas, `v1/nearest`, `v1/table` (etiquetas muchos-a-muchos, 1.000.000 de celdas como máximo) y `v1/status`; `openapi.yaml` y demo; packs `es` y `lu` leídos en tiempo de ejecución, con etapas por extracto; tráfico en vivo de los sensores de Madrid, opcional y desactivado por defecto, con `traffic.coverage`.

**Primera medición contra OSRM (2026-11-03, Baleares, OSRM v26.11.0, una sola máquina):** geko, OSRM y `wrk` en contenedores con la misma red, servidores en 8 núcleos, 16 conexiones con keep-alive, mismas URLs y rondas alternas. Se descarta una ronda de OSRM CH que duró 9,66 min, probablemente por una suspensión del equipo.

| | geko | OSRM CH | OSRM MLD |
|---|---|---|---|
| Sin geometría, peticiones/s | 9.061–9.866 | 9.343–9.579 | 7.489–7.678 |
| Sin geometría, p99 | 3,1–3,4 ms | 2,5–2,9 ms | 3,0 ms |
| Con geometría, peticiones/s | 5.103–5.702 | 3.578–4.044 | 2.946–3.498 |
| Con geometría, p99 | 5,4–6,6 ms | 6,9–8,5 ms | 7,9–13,3 ms |
| RAM en reposo | 724 MB | ~85 MB | ~85 MB |
| RAM tras la carga | 851 MB (pico 1,1 GB) | 101 MB | 115 MB |

Esa RAM de geko era un fallo del índice de calles, ya corregido (punto 1). Calidad, 1.000 pares contra OSRM CH: distancia con mediana 0,999 (el 86,5 % a menos del 2 %), duración con mediana 0,800. CCH coincide con Dijkstra en todos los pares comprobados. «Más rápido que OSRM» sigue sin confirmarse.

**Medición de España completa (2026-11-04, `ci/ab-osrm.sh`, `MIN_LAT=30`, 3 rondas):** sin Canarias en las consultas, porque OSRM lee un solo extracto. Es anterior a `query_many` (punto 4).

| | geko | OSRM CH | OSRM MLD |
|---|---|---|---|
| Sin geometría, peticiones/s | 866–946 | 1.882–2.371 | 1.625–1.766 |
| Sin geometría, mediana | 16,4–17,8 ms | 6,5–7,6 ms | 8,9–9,5 ms |
| Con geometría, peticiones/s | 673–747 | 685–760 | 703–754 |
| RAM en reposo | 3,4 GiB | 3,0 GiB | 3,0 GiB |
| RAM tras la carga | 4,9 GiB (pico 5,3) | 3,0 GiB | 3,2 GiB |
| Disco | 1,1 GB | 4,1 GB (CH y MLD) | la misma |

Generación del router: 8,3 M de aristas, 14,6 M de giros, 408 s y 3,1 GB de pico. OSRM: `osrm-contract` 333 s. Carga de geko: unos 30 s. En proceso, con un punto por extremo: ruta con CCH 1,9 ms de mediana y Dijkstra 904 ms, sin diferencias en 100 pares. Customización: 12 s con un hilo, 4,2 s con 16.

**Medición de Baleares tras `query_many` (2026-11-04, otro equipo: 12 núcleos y 12 GB, servidores en los núcleos 0-7 y `wrk` en 8-11, un servidor cada vez, 3 rondas):** no es comparable con las tablas anteriores, porque OSRM CH da aquí 5.200–6.000 peticiones/s sin geometría frente a 9.300–9.600 allí.

| | geko | OSRM CH | OSRM MLD |
|---|---|---|---|
| Sin geometría, peticiones/s | 8.317–12.606 | 5.241–5.975 | 4.261–4.843 |
| Sin geometría, p99 | 3,2–5,7 ms | 7,7–11,5 ms | 7,3–9,6 ms |
| Con geometría, peticiones/s | 4.146–5.569 | 2.014–2.458 | 905–1.982 |
| Con geometría, p99 | 6,2–18,5 ms | 16,7–31,7 ms | 26,8–63,3 ms |
| RAM en reposo | 134–146 MB | ~87 MB | ~87 MB |
| RAM tras la carga | 290–317 MB (717 con geometría) | 98–103 MB | 109–116 MB |

`wrk` marcó 32–128 errores de lectura por ronda en OSRM y 10 y 4 `timeouts` en dos rondas de geko; no se investigó la causa. La primera ronda de geko sin geometría (12.606) y la segunda (8.317) difieren mucho: el orden entre servidores se repite, la cifra exacta no.

**Efecto de `query_many` (misma sesión, Baleares, pares alternos de `9a41f87` y `ad3cec1`):** sin geometría 1,87, 1,54 y 1,51 veces las peticiones/s; con geometría 0,99, 1,41 y 1,19. Mejora en 5 de 6 pares y empate en uno. El equipo se fue cargando durante la prueba (la cifra absoluta del binario nuevo bajó de 7.193 a 3.286 entre rondas), así que vale la razón dentro de cada par, no los valores absolutos. España sigue sin repetirse: `osrm-extract` de España gastó casi toda la memoria de este equipo y se descartó.

### A. Base sólida y medida

- [x] 1. Reducir la memoria y el tiempo de carga. El índice de calles guardaba los ferris, que llenaban miles de celdas: en Baleares, de 690 a 98 MB y de 4,7 a 0,47 s (`ad302b7`). En España, 3,4 GiB en reposo frente a 3,0 de OSRM.
- [x] 2. Pasar a `mmap` el grafo y la jerarquía: **descartado por medición** (2026-11-04). La lectura es el 10 % de la carga de España (~2 s de ~20) y lo que pesaba era el cálculo, que se recortó en `Snapper` (`b6a262c`); el `mmap` queda como idea si algún día importa compartir la caché de páginas entre procesos. Lo que había: sin empezar: el formato no está alineado y `Graph` usa `Vec`, así que es un cambio grande. Los 30 s de carga de España son el motivo para hacerlo. Medido el 2026-11-04 en otro equipo (12 núcleos, WSL), con temporizadores en una copia y el equipo en reposo, la carga de España en `geko serve` tarda 20,3 s: `graph_io::read` 0,9 s; `turn::build` 2,3 s; `scc` y `Snapper` 8,1 s; `Cch::read` 1,1 s; `customize` 7,8 s (de ellos, `Lower::new` 1,4 s). Un banco en proceso, con el mínimo de 3 repeticiones, da lo mismo (unos 16 s: 0,3–0,5; 1,9; 7,0–9,4; 0,55; 5,5–7,2). La lectura es solo el 10 % de la carga, así que el `mmap` ahorra a lo sumo 2 s de 20, y lo que pesa es el cálculo: `scc`/`Snapper` y `customize`, casi el 80 %. Un primer intento, con el equipo ocupado, daba 42–75 s y hacía parecer que la lectura era la mitad: era ruido, y la comparación del lector nuevo con el antiguo no mostró diferencia medible (0,48 s frente a 0,48–0,54 s). Mirar antes el cálculo, y repetir siempre con el equipo en reposo. Dentro del cálculo, `scc` son solo 0,56 s y `Snapper::new` 6,8 s (0,9 s `by_shape` y 6,0 s las celdas: 26,7 M de inserciones en 2,4 M de celdas). Recorrer las formas en el orden del grafo, y no en el del `HashMap`, lo deja en 5,0 s; repartir las celdas en 16 bandas de filas, cada una en su hilo, en 2,4 s. Un hasher rápido por sí solo no ayudó (7,1 s). Efecto lateral: el índice es ahora determinista. Con el `HashMap` de semilla aleatoria, ante dos calles equidistantes en un cruce, dos arranques seguidos daban resultados distintos en 118 de 400 rutas de Baleares (distinta `duration_s` por el coste de giro de la arista elegida); con el índice nuevo, 0 de 400. Arranque de Baleares: 0,37–0,92 s antes y 0,30–0,47 s ahora. Pendiente medir el arranque completo de España con la versión nueva.
- [x] 3. Añadir mimalloc en la `.so` del módulo (`beeb1b7`). Sin medir su efecto por separado.
- [ ] 4. **Aplazado por decisión del propietario (2026-11-04).** Medir España completa contra OSRM. Hecha una primera vez (arriba): los hilos sí ayudan en la customización, y sin geometría geko va 2,5 veces más lento que CH. La causa más clara era una búsqueda por cada combinación de sentidos, hasta 4 por ruta, ya corregida con `query_many`, que además quita un tercio del estado de cada consulta. **Falta repetir la medición de España** con ese cambio (en Baleares, 2026-11-04, `query_many` multiplica las peticiones/s por 1,5 a 1,9 sin geometría en 3 de 3 pares, y por 1,2 a 1,4 con geometría en 2 de 3, con un empate; ver arriba): se paró por falta de memoria del sistema con los tres servidores a la vez (unos 9 GB), sin llegar a dar datos. Conviene repetirla con la máquina libre, o con un servidor cada vez. Si sigue por detrás, probar un orden por disección anidada: el de grado mínimo da 57,8 M de atajos y profundidad 2.115 en España.
- [x] 5. Calibrar las duraciones: alineadas con `car.lua` de OSRM (2026-11-04). El pack lleva ahora sus velocidades por clase (motorway 90, primary 65, residential 25…) y `maxspeed_factor = 0,8` sobre `maxspeed`, que sustituye a la de la clase. En Baleares, 1.000 pares contra OSRM CH: duración con mediana 0,878 → 0,996 (p5–p95 de 0,714–0,978 a 0,969–1,024) y distancia a menos del 2 % del 88,6 % al 94,5 %. Sigue siendo una referencia, no tiempos reales: si se consiguen (trazas GPS), se recalibra.
- [x] 6. Revisar los pares con más diferencia de distancia (2026-11-04). Con las velocidades de `car.lua` (punto 5) el peor ratio pasa de 0,667 a 0,797 por abajo y llega a 1,203 por arriba: la diferencia grande era del modelo de velocidades. Lo que queda (el 5,5 % de pares a más del 2 %) se mira con `diff` en los dos peores y es del objetivo: OSRM minimiza un peso (penaliza `service`, `side_road` y giros) y nosotros la duración, así que en un par elegimos una vía rápida más larga (49,8 km, 3.002 s frente a 41,4 km, 2.772 s de OSRM) y en otro una más corta (4,1 km frente a 5,2 km). No hay indicios de fallo. No se vio en un mapa: se comparó con `diff` en texto. Para comparar con justicia habría que pedir a OSRM la duración de nuestra ruta.
- [x] 7. Sustituir el ejemplo `load` por `urls` y `ci/ab-osrm.sh`, con `wrk` en igualdad de condiciones.

### B. API de la fase 1

- [x] 8. **[decisión]** Formato de la API: propio, documentado en `openapi.yaml` (punto 12). Sin capa de compatibilidad con OSRM.
- [x] 9. Alternativas y pasos en `route`.
  - Nombres de calle en el grafo: `Edge.name` (índice en `Graph::names`, `name` o si no `ref`); el fichero pasa a `GEKORG04` (hay que reconstruir el grafo con `geko build --force`).
  - Pasos (`?steps=true`, módulo `steps`): `depart`, `turn` (con `modifier`: slight/sharp left/right, left, right, uturn), `new name` (la calle cambia y se sigue recto) y `arrive`; cada uno con `name`, `distance_m`, `duration_s` y `location`. Se leen del ángulo entre la arista que llega y la que sale, y de sus nombres. Sin glorietas, salidas ni carriles todavía.
  - Alternativas (`?alternatives=n`, n ≤ 3, `Cch::meeting_points` + `route::alternatives_cch`): cada nodo donde se encuentran las dos búsquedas da un camino; se admite el que no pasa de un 25 % más largo que el más rápido, comparte como mucho el 80 % de sí mismo con cada ruta ya dada y es localmente óptimo (tramo de un cuarto de su duración en torno al nodo, a menos de un 5 % del camino más corto entre sus extremos). Solo para puntos en aristas distintas.
  - Validación (Baleares, 116 rutas aleatorias de Mallorca): los pasos suman exactamente la distancia y la duración de la ruta; 85 de 116 rutas tienen alguna alternativa (137 en total), ninguna pasa de 1,25× de la más rápida.
  - Coste (300 pares aleatorios, mediana de `compute_us`, dos rondas): ruta sola 231–257 µs; con pasos 296; con 2 alternativas 685–779 µs (p99 2,1–2,5 ms); con ambas 860–889 µs.
  - Test: `a_route_has_the_alternative_that_is_not_too_long_and_not_the_same`, los de `steps`, y el de lectura/escritura del grafo con nombres.
- [x] 10. `table` con un algoritmo muchos-a-muchos, hasta 1.000×1.000, y segunda medición.
  - Algoritmo: cada punto da una etiqueta (`Cch::label`: distancias hacia arriba a todos sus ancestros en el árbol de eliminación); una celda es la mejor suma sobre los ancestros comunes (`Label::meet`). Las filas se reparten entre hilos.
  - `GET /v1/table` (puntos repetidos `from`/`to`) y `POST /v1/table` con cuerpo `{"from":[[lat,lon],…],"to":[…],"snap_m":n}`. Límites: 1.000.000 celdas y 2.000 puntos por lado.
  - Medición (Baleares, 1.000×1.000 aleatorios en Mallorca, un `POST`, máquina en reposo): 0,25–0,44 s de cómputo, 0,29–0,38 s totales con 6 MB de respuesta. El método anterior, una consulta por celda (≈230 µs cada una en `route`), costaría unos 230 s: ≈500–900 veces más.
  - Exactitud: 1.875 celdas de `POST /v1/table` contra `GET /v1/route` por pares, 0 diferencias (tolerancia 0,1 s del redondeo).
  - Fallo hallado y corregido en la validación en vivo: `label` suponía limpio el `Scratch`, pero `query_many` lo limpia al empezar la siguiente consulta; tras una ruta, la primera tabla salía errónea. Prueba `labels_after_a_query_are_those_of_a_clean_scratch`.
- [x] 11. Índice espacial del plan (R-tree de Hilbert), o decidir que basta la rejilla actual. **Decidido: basta la rejilla.**
  - Medición con `examples/snapbench` (Baleares, 200.000 puntos, índice de 91–120 ms):
    - Cerca de calles: mediana 13 µs, p90 27–28 µs, p99 57–60 µs, media 16 µs, con 100 m o 1.000 m de límite.
    - Puntos cualesquiera del rectángulo (casi todos en el mar o el monte): mediana 0,5 µs con 100 m; 3,4 µs con 1.000 m (p99 11 µs).
  - Una ruta cuesta unos 230 µs, así que el ajuste a la calle es menos del 10 % de una petición y no es el cuello de botella. Un R-tree no lo cambiaría de forma apreciable; queda descartado.
  - Máximos aislados de 6–7 ms (celdas densas o ruido de la máquina), no repetidos en el p99.
- [x] 12. `openapi.yaml` y demo en `/router/`.
  - `openapi.yaml` en la raíz del repo (como el del geocoder), servido en `/router/openapi.yaml` y listado en `/docs` del host (comprobado en vivo). Documenta `status`, `route` (con `steps` y `alternatives`), `nearest` y `table` (GET y POST). Es un YAML válido (cargado con PyYAML).
  - Demo en `/router/` (ruta `""` pública, `module/src/demo.html`, autocontenida, sin peticiones fuera del servidor ni mapa base): clic en el mapa para origen y destino, zoom y arrastre, rutas y alternativas dibujadas, pasos en palabras. Pide `v1/status` (que ahora da `bounds`) para encuadrar. Ambas páginas se sirven aunque no haya grafo cargado.
  - **Sin verificar en un navegador**: la extensión de Chrome no estaba conectada y no hay `node` para comprobar la sintaxis del script. Solo está probado que el servidor la sirve (200, `text/html`) y que no menciona URLs externas.

### C. Packs de país

Comprobado el 2026-11-04 con el binario de `geko` y la `.so` del módulo (Madrid y Luxemburgo en un ámbito): sin packs falla con «not published, or not public»; con `GEKO_ROUTER_PACKS`, `plan`, `build` y `serve` funcionan, y responde rutas de los dos países desde un solo grafo.

- [x] 13. Cargador de packs (`module/src/pack.rs`).
  - Lee `pack.toml`, `data/` y `sources/` en tiempo de ejecución y valida cada fichero, nombrándolo en el error. `countries.toml` y `road-classes.toml` ya no van incrustados.
  - Baja los del ámbito en `prepare` con `geko-artifact` a `<datos>/router/packs/<país>/`; `geko purge packages` borra los de fuera del ámbito. Una copia a mano se respeta, y `GEKO_ROUTER_PACKS` lee de una carpeta sin descargar.
  - Un país sin pack es un error, como en el geocoder. Antes se podía generar cualquier país con las velocidades incrustadas.
  - Cada extracto se lee con el perfil de su país, así que un ámbito de varios países usa las velocidades de cada uno.
  - `geko serve` lee los packs para las fuentes de tráfico y no los baja: si faltan, avisa y sirve sin tráfico. El bundle guarda sus versiones en `meta.packs`.
- [x] 14. Opciones en `geko-router.toml`: `[packs] registry`, `[profiles] default`, `[traffic] live`, `[sources.<nombre>]` y `[serve] preload`. Detalle en «Configuración». `[serve] countries` se descartó, y `[traffic.madrid]`, `interval_secs` y `max_age_secs` desaparecen sin compatibilidad, porque el módulo no está publicado.
- [x] 15. Pack `es` completo: `regions.toml`, `region-boxes.tsv` (copiados del geocoder), `tests/pack.rs` y golden.
  - Golden (`ci/golden.sh`): 200 rutas en `es` (100 de Baleares y 100 de Madrid con una lectura congelada del feed), cada una con su duración y distancia. Dos pasadas dan 0 diferencias. En Madrid, 13 de 100 rutas se alargan con tráfico: el feed se congeló a las 23:29, sin apenas retenciones.
  - **Lo que no prueba:** los números son los que da hoy el router, así que detectan cambios, no aciertos. Solo las 9 rutas escritas a mano llevan un rango (`expect_km`), y es una estimación mía, no una medición en un mapa: amplié a 30–50 km el de Móstoles–Alcobendas al dar 47,3 km por la M-50. No se han comparado con OSRM, porque esta máquina no tiene Docker.
  - Los datos congelados (`golden/snapshot`) están en el paquete privado `ghcr.io/candymaps/geko-router/golden`, y el job `golden` de `ci.yml` los baja y pasa las rutas en cada PR que cambie lo que se compila.
- [x] 16. Pack `lu`: solo OSM, con su calendario de festivos, una partición y 100 rutas de golden (4 a mano). El extracto sale del índice de Geofabrik, sin `osm.toml`. Las velocidades son una copia de las de `es`.
- [x] 17. Particiones con sellos, **adaptado** (2026-11-04): por extracto, no por provincia.
  - La CCH es global, porque una ruta cruza provincias, y un PBF nuevo de España cambia todas las provincias a la vez: un sello por provincia no ahorraría nada con el país entero.
  - Etapas: un fragmento por extracto, el grafo y la jerarquía, cada una con su sello. Con todo sellado, Baleares y Luxemburgo tardan 2 ms; al cambiar las velocidades de `lu`, solo se relee Luxemburgo (4,3 s frente a 24 s con la descarga).
  - Un ámbito de provincias recorta a sus cajas: Málaga (`ES-MA`) da 138.175 nodos, frente a 597.010 de Andalucía (`ES-AN`), y 2 regiones del mismo extracto comparten una lectura.
  - España completa, sin cambios en el grafo: 3.793.285 nodos, 8.287.395 aristas y 14.590.189 giros, los mismos que antes de los fragmentos. Tarda 253 s, con pico de 3,2 GB (fragmento de `spain` 151 s, grafo 37 s, jerarquía 61 s). La cifra anterior (408 s, 3,1 GB) es de otra sesión, así que no prueba una mejora de tiempo. Los fragmentos son trabajo en disco, no van al bundle.
  - Una caja es un rectángulo: incluye algo de las provincias vecinas, y una vía se corta en su borde.
- [x] 18. Publicación firmada de los packs en ghcr. Hecha el 2026-11-05: `es` y `lu` están públicos en `ghcr.io/candymaps/geko-router/countries/<país>`, con `format-1` y `pack-1`.
  - `ci/packs.sh`, `ci/oras.sh`, el binario `geko-router-pack-sign` y `.github/workflows/publish.yml`, en cada push a `main`. `GEKO_PACKS_DRY_RUN=1` firma los dos packs con una clave de prueba.
  - Comprobado con `geko` y la `.so` del módulo, sin `GEKO_ROUTER_PACKS` y en una carpeta de datos vacía: baja y verifica los dos packs sin credenciales, y `plan`, `build` y `serve` responden rutas de Madrid y Luxemburgo.
  - `pack-1` ya no se puede cambiar: cualquier cambio en los ficheros publicados de un pack necesita subir su `version`.

### D. Tráfico completo

- [ ] 19. Probar la capa en vivo con el feed real de Madrid, de punta a punta. Prueba de humo el 2026-11-04, con `geko serve` y el feed real: 1.248 aristas medidas, 127 lentas y una ruta del centro que pasa de 695 a 711 s. Queda abierto: no se ha comprobado el casado sensor a sensor ni el comportamiento en horas punta.
- [ ] 20. Calibrar la conversión de nivel de servicio a velocidad (valores provisionales).
- [ ] 21. Proveedores genéricos por `kind`. La URL, la cadencia, la caducidad, la licencia y lo que cubre Madrid ya salen del pack; falta que `madrid.rs` lea del pack las etiquetas y los factores de nivel de servicio, que siguen en el código.
- [ ] 22. Casar los sensores al generar el bundle, no en servicio.
- [ ] 23. Combinar varias fuentes: hoy hay un único poller y cada lectura sustituye a la anterior.
- [ ] 24. Perfil histórico: mediciones, velocidad por hora y día, calendario de festivos, y decaimiento hacia el histórico en vez de a velocidad libre. Como mete datos de Madrid (CC BY) en el bundle junto al grafo de OSM (ODbL), hay que declarar en el manifiesto qué lleva cada licencia. El código sigue siendo AGPL; hoy no hay mezcla, porque el tráfico en vivo no va en el bundle.
- [ ] 25. `traffic=none|historical|live` y `depart_at`, con la parte histórica en `traffic.coverage`.
- [ ] 26. Incidencias: parser DATEX II y cierre real de vías (hoy lo más lento es un 5 % de la velocidad). La fuente de la DGT queda desactivada por defecto hasta cerrar el punto 28.
- [x] 27. **[gestión]** Leer las condiciones de uso de Madrid y la ficha del conjunto de histórico. Hecho el 2026-11-05, después de publicar `pack-1`, que solo declara la fuente (ver «Licencias»). Revisión de lectura, no asesoramiento jurídico.
- [ ] 28. **[gestión]** Escribir a la DGT para aclarar su licencia. Es una precaución, no un permiso previo: no bloquea el punto 26 y solo condiciona activar la fuente por defecto.

### E. Funciones de la fase posterior

- [ ] 29. `match` (map matching).
- [ ] 30. Isócronas.
- [ ] 31. `trip`.
- [ ] 32. Perfiles de bici, a pie y camión.

### F. Publicación

- [x] 33. CI del módulo: `ci.yml`, `pr-comment.yml`, `protected.yml`, `ci/test.sh`, `ci/affected.sh` y `deny.toml`, como en los tiles.
  - `ci/test.sh` pasa clippy, reglas de dependencias, `cargo-deny`, tests y conformidad. Pasa en WSL; en GitHub aún no se ha ejecutado, porque solo corre en un PR.
  - La protección de `main` y `dev` exige `test`, `golden` y `protected`.
- [ ] 34. Publicar el módulo firmado y añadirlo a `ci/modules` del host.
- [ ] 35. README, `AGENTS.md`, sección «Optimizaciones» y referencia de opciones del módulo.
- [ ] 36. Actualizar PROJECT.md y el README del host con el módulo nuevo. Este plan vive en este repositorio, como lo propio del módulo.
- [x] 37. PR de `feature/router-module` a `dev`, y de `dev` a `main`. **Adelantado** (2026-11-05) para publicar los packs: los puntos 34 a 36 y los bloques D y E siguen abiertos, y llegarán a `main` con sus propias PR.

### Propuesta de corte de la v1

**[decisión]** Bloques A, B y F, más los puntos 13–15 del pack `es`, con el tráfico de Madrid desactivado por defecto. El resto de C y D iría en versiones siguientes, y E al final.

## Riesgos

- **Cobertura de tráfico escasa y desigual.** Mitigación: tres capas con degradación y `traffic.coverage` explícito.
- **Casar sensores con aristas mal** da retenciones fantasma. Mitigación: casado conservador (sentido, clase de vía, distancia máxima), con test por sensor.
- **Licencias de las fuentes** sin confirmar (DGT, cita.lu): sin licencia clara no se empaqueta.
- **Una fuente que cambia de formato o se da de baja** (varios conjuntos de la DGT figuran como obsoletos): el pack es versionado y una fuente caída degrada, no rompe.
- **Memoria y tiempo de generación** de CCH con España completa: se mide en el paso 3 antes de seguir.
- **Un fallo grave del módulo tira geko** (`DylibModule`, MODULES_PLAN.md, «Riesgos»).

## Decisiones abiertas

- **Alcance de la v1:** solo coche, `route`/`table`, sin tráfico (recomendado: la base medida primero).
- **Primer país con tráfico:** Madrid (CC BY 4.0, sensores, el dato más claro) frente a la DGT (cobertura estatal, solo incidencias, licencia por confirmar).
- **Calendario por país:** los festivos nacionales están en `calendar.toml`; queda por decidir de dónde salen los regionales y locales.
