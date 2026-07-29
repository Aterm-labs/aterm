# Sesiones compartidas (v1)

Que alguien reanude la sesión de un compañero pulsando un botón, sin export/import
manual y preservando el id original. Portado del diseño validado en multi-claude
(`docs/REMOTE-SESSIONS.md` de aquel repo) y adaptado a lo que este proyecto tiene y
aquel no: **varios proveedores** y **dos interfaces** sobre el mismo core.

El requisito que **no** hay que resolver: cada persona usa su propia cuenta del agente.
El repositorio guarda transcripts, no credenciales.

## Por qué no basta con montar el directorio en red

- **El nombre de la carpeta lo deriva el agente del cwd local.** Dos personas con
  `$HOME` distinto escriben en carpetas distintas del mismo montaje, así que
  `claude --resume` nunca ve las sesiones de la otra. Se comparte el disco sin
  compartir nada.
- **Si los paths coinciden, es peor**: dos procesos hacen `append` al mismo `.jsonl`.
  En local es atómico para líneas pequeñas; sobre NFS/SSHFS no está garantizado y el
  fichero se corrompe a nivel de bytes.

Conclusión: local-first. El repositorio es un almacén, nunca el directorio de trabajo,
y la herramienta hace de **traductor de identidad** entre el id del compañero y el
layout local del proveedor.

## Decisiones cerradas

| Tema | Decisión |
|------|----------|
| Dónde vive el motor | Crate `agent-sessions-remote` + comandos `remote-*` del sidecar. **Community**: las dos UIs lo consumen y un repositorio publicado desde una se lee desde la otra |
| Qué se gatea con Pro | Solo la UI de la extensión (publicar, pestañas de repositorio, traer, gestionar servidores/enlaces), igual que comparativa paralela y plantillas |
| Vendor intacto | `agent-sessions` sigue siendo copia verbatim y read-only; el crate nuevo se apoya en su trait (`locate`, `transcript`, `resume_argv`) sin tocarlo |
| id | Se preserva tal cual: es lo que acepta el CLI al reanudar y los uuid no colisionan entre personas |
| `cwd` embebido | No se reescribe: es histórico y el agente lo ignora |
| Destino al hidratar | El **proyecto abierto**, elegido por quien trae la sesión — nunca el cwd del manifest, que es de quien publicó y aquí puede no existir |
| Backends | Carpeta, git por SSH, y API REST de GitLab/GitHub |
| Servidores | Definidos una vez; los enlaces los referencian **por nombre**, no copian su host |
| Alcance | **Por proyecto**: cada uno se enlaza a uno o varios repositorios, cada uno una pestaña. El global solo es fallback |
| Clave del enlace | El `origin` normalizado, así que todos los worktrees del repo comparten enlace |
| Credenciales | Un fichero por servidor con permisos `0600`, o `$ATERM_REMOTE_TOKEN`; **nunca** en `remotes.json` |
| Compresión | gzip (stdlib de `flate2`), salvo los `.meta.json` de unos cientos de bytes |
| Manifests | Uno por sesión, nunca un manifest global (evita conflictos de escritura) |
| Concurrencia | Fork explícito pendiente; hoy republicar sobrescribe (ver «Límites») |
| Cifrado | Ninguno en v1: el control de acceso lo dan los permisos del repositorio |

## Qué compone una sesión, y qué viaja

| Ruta | ¿Viaja? | Por qué |
|------|---------|---------|
| El transcript del proveedor | Sí | Es la conversación |
| `<id>/subagents/**` | Sí | En sesiones con fan-out es la mayor parte del trabajo |
| `<id>/tool-results/**` | Sí | Salidas grandes volcadas fuera del transcript; sin ellas hay agujeros |
| `<proyecto>/memory/` | **No** | Auto-memoria personal |
| `session-env` | **No** | Puede contener secretos de máquina; el agente lo recrea |
| Enlaces simbólicos | **No** | Publicarían aquello a lo que apuntan |

Lo que **no** viaja y no tiene solución: el código. Ver «Divergencia de código».

## Multi-proveedor: qué es reanudable y qué no

Esta es la diferencia grande respecto al original, que era solo-Claude. Cada agente
guarda sus sesiones con otra forma, así que el manifest lleva `provider` y la
hidratación elige el layout de destino:

| Proveedor | Publica | Se trae a disco | Dónde aterriza |
|-----------|---------|-----------------|----------------|
| claude | el `.jsonl` + su directorio hermano | sí | `~/.claude/projects/<encode_cwd(destino)>/<id>.jsonl` |
| codex | el rollout | sí | `~/.codex/sessions/YYYY/MM/DD/<nombre original>` (la fecha sale del nombre, que viaja en el manifest) |
| qwen | el `.jsonl` | sí | `~/.qwen/projects/<encode_cwd(destino)>/chats/<id>.jsonl` |
| gemini | el `.jsonl` | sí, con una condición | `~/.gemini/tmp/<id corto>/chats/…` — Gemini inventa ese id corto y lo apunta en `projects.json`, así que solo podemos escribir en un proyecto que Gemini ya conozca. Si no lo conoce, se dice y no se escribe nada |
| goose | la conversación renderizada | **no** | vive en SQLite; se puede leer, no reanudar |
| opencode | la conversación renderizada, si su CLI la da | **no** | no hay fichero por sesión |

Los dos últimos se publican con `resumable: false`, y traerlos falla *antes* de tocar
el disco con un mensaje que lo explica, en lugar de escribir algo que el CLI ignoraría.

## Layout en el repositorio

```
manifest/<id>.json
blobs/<id>/session.jsonl.gz
blobs/<id>/sub/subagents/agent-*.jsonl.gz
blobs/<id>/sub/subagents/agent-*.meta.json
blobs/<id>/transcript.json.gz        # proveedores no reanudables
```

El layout, el gzip y el orden viven en `store.rs`, no en los backends, así que una
sesión publicada en una carpeta y la misma publicada en GitLab son **idénticas byte a
byte** y un repositorio se puede leer con cualquiera de los tres drivers. Un backend
nuevo solo aporta cuatro operaciones: listar, leer, escribir y borrar un fichero.

`manifest/<id>.json` (snake_case a propósito: se lee a mano y en la vista web del repo):

```json
{
  "format": "aterm/remote-session",
  "version": 1,
  "id": "…", "provider": "claude",
  "published_at": "2026-07-29T13:56:24+00:00",
  "published_by": "ana@factorlibre.com",
  "cwd": "/home/ana/WS/repo", "branch": "main",
  "git_remote": "git.empresa.com/odoo-16/fl-v16", "git_head": "abc1234",
  "display_name": null, "tags": [], "first_prompt": "¿por qué falla el pago?",
  "message_count": 412, "size_bytes": 1234567,
  "resumable": true, "origin_filename": "…",
  "artifacts": [{"path": "session.jsonl", "bytes": 210, "gzip": true}],
  "forked_from": null
}
```

Dos detalles con motivo:

- **`artifacts` va en el manifest** en vez de descubrirse listando el remoto, porque la
  API de contenidos de GitHub no lista recursivamente y habría que recorrerla petición
  a petición.
- **Los dos órdenes de escritura son la única garantía de atomicidad que hay.**
  Publicar escribe los blobs primero y el manifest al final; despublicar borra el
  manifest primero y los blobs después. Como el manifest es lo que hace visible una
  sesión, una transferencia interrumpida en cualquiera de los dos sentidos deja blobs
  sin referenciar —invisibles e inocuos— nunca una entrada apuntando a payload que ya
  no está. (En el backend de git da igual: un commit es atómico.)

## Arquitectura

```
crates/agent-sessions-remote/
    manifest.rs   el documento que hace visible una sesión
    payload.rs    qué viaja y dónde aterriza, por proveedor
    store.rs      layout + gzip + las 4 operaciones que aporta un backend
    directory.rs  carpeta (también el doble de test: CI sin red)
    git.rs        git por SSH (clon en caché, push con rebase, ssh -T)
    http.rs       GitLab y GitHub por API REST (vía curl)
    links.rs      servidores, enlaces por proyecto, tokens 0600

crates/agent-sessions-cli/src/remote.rs
    remote-config | remote-server-set | remote-server-delete
    remote-links  | remote-links-set  | remote-global-set
    remote-probe  | remote-list       | remote-plan
    remote-publish| remote-fetch      | remote-unpublish | remote-shared
```

Sin dependencia HTTP pesada: los drivers de API llaman a `curl` igual que hace
`service_status` de la app nativa, y el de git llama al binario `git`. Ambos ya están
en cualquier máquina que use esos backends, y el sidecar sigue siendo pequeño.

### Drivers de GitLab y GitHub

Vía API REST, sin clonar. Los dos proveedores se reducen a las mismas operaciones con
endpoints distintos, y dos asimetrías que el código absorbe:

- **GitLab no tiene upsert**: crear un fichero que ya existe es un 400, así que la
  escritura reintenta como `PUT`. Sin eso, republicar fallaría siempre.
- **GitHub exige el `sha` del blob** para sobrescribir, lo que obliga a un `GET` previo.

**Un commit por fichero, no uno por sesión.** El endpoint multi-acción de GitLab haría
atómica la publicación, pero GitHub no tiene equivalente directo y mantener dos caminos
distintos por proveedor duplicaba la parte más delicada. El coste es un historial más
ruidoso; la invariante que importa se conserva con el manifest al final.

### SSH frente a token

| | Token (API REST) | SSH (`git.rs`) |
|---|---|---|
| Credencial | Una por persona y host, hay que crearla y repartirla | Las claves que ya están desplegadas |
| Transporte | `curl` contra la API | El binario `git` sobre un clon en `~/.cache/aterm/remote-repos/` |
| Concurrencia | La segunda publicación **pisa** la primera | Push rechazado → rebase → reintento: **ambas sobreviven** |
| Coste | Ninguno en disco | Una copia de trabajo por repositorio y rama |

Esa fila de concurrencia es la razón técnica para preferir SSH.

Tres detalles del driver de git que no son obvios:

- **`LC_ALL=C` es obligatorio.** git traduce sus errores, así que en un sistema en
  español «repository does not exist» llega como «el repositorio no existe» y cualquier
  interpretación del stderr deja de funcionar en silencio.
- **`GIT_TERMINAL_PROMPT=0` y `BatchMode=yes`.** Una petición de credenciales colgada
  dentro de un worker es invisible y parece que la aplicación se ha congelado.
- **`git add -A`, no `git add -- manifest blobs`.** En cuanto un borrado ha eliminado
  ambos directorios, nombrarlos falla con «pathspec did not match any files»; `-A`
  además registra borrados igual que altas.

### Comprobar acceso SSH: `ssh -T`, no un repositorio inventado

`ssh -T` es lo que GitHub y GitLab esperan: no necesita repositorio y ambos responden
con el nombre de la cuenta. Dos detalles:

- **GitHub sale con código ≠ 0 al autenticar correctamente** («does not provide shell
  access»), así que el código de salida no dice nada y solo sirve el saludo.
- El error de clave rechazada **nombra la equivocación probable**: el usuario SSH es
  siempre `git`, y confundirlo con la cuenta es el fallo natural.

Y **el puerto SSH no se puede deducir**: la URL web contesta por 443 tanto si SSH está
en el 22 como en el 2211, y la forma `git@host:grupo/repo.git` no puede expresar un
puerto (lo que sigue a los dos puntos es la ruta), así que uno no estándar obliga a la
forma explícita `ssh://`. Cuando falta, el síntoma es el peor posible —el 22 no
responde *nada*—, así que el silencio en el 22 sugiere explícitamente revisarlo.

## Estado de la copia local

La pestaña de un repositorio lista **todo** lo publicado, no solo lo que no tienes:
ocultar lo que ya está en disco parecía evitar duplicados, pero rompe lo primero que
uno quiere después de publicar — ver su sesión ahí como confirmación.

| Marca | Estado | Cómo se decide |
|-------|--------|----------------|
| `☁` | `absent` | No hay copia local con ese id |
| `✓` | `current` | El tamaño local coincide con el publicado |
| `↻` | `stale` | El manifest declara más bytes: alguien la continuó y republicó |
| `↑` | `ahead` | El local tiene más bytes: has seguido trabajando sin publicar |

Comparar tamaños basta porque los transcripts son append-only: cualquier diferencia es
contenido real, no una reescritura. El coste es un `stat` por fila.

Las **mismas cuatro marcas** aparecen en la lista local (`remote-shared` construye el
índice `provider:id → repositorio`), con la autoría cuando quien publicó no eres tú —
que es lo que distingue «mía y compartida» de «traída de un compañero».

## Servidores y enlaces

Un servidor es proveedor + URL + autenticación; un enlace es repositorio + rama +
nombre de pestaña **sobre** un servidor. Están separados porque cambian a ritmos
distintos: una empresa tiene uno o dos servidores y un repositorio por cliente, así que
teclear el host y pegar un token en cada uno era trabajo repetido que dejaba la misma
credencial en varios sitios. Los enlaces referencian el servidor **por nombre**, así que
rotar un token arregla todos los repositorios que apuntan a él. Un enlace que nombra un
servidor inexistente resuelve a un error: inerte y visiblemente inerte, mejor que
publicar en otro sitio sin avisar.

Resolución, primero que gana:

1. `ATERM_REMOTE_DIR` — override total a una carpeta, para probar sin configurar nada.
2. Los enlaces propios del proyecto.
3. Los enlaces globales.

Los propios ganan **por completo** sobre el global, no se suman: un proyecto enlazado al
repositorio de un cliente no debe publicar además al repositorio por defecto.

La clave es el `origin` normalizado (`git@host:g/r.git`, `https://host/g/r.git` y
`ssh://git@host:2211/g/r.git` dan la misma), con la ruta absoluta como fallback. Dos
consecuencias buscadas: todos los worktrees de un repositorio comparten enlace, y nadie
tiene que enlazar el mismo repositorio dos veces.

Ficheros: `~/.config/aterm/remotes.json` (servidores + enlaces) y
`~/.config/aterm/remote-tokens/<servidor>` (un token por fichero, `0600`).

## Divergencia de código

El transcript viaja; el repositorio no. Si la sesión se grabó sobre `abc1234` y tú estás
en `def5678`, la conversación describe ficheros que ya no son esos.

v1 lo mitiga, no lo resuelve: al traerla se compara `git_remote`/`git_head` con el
estado local y se avisa antes de lanzar. Un prefacio inyectado en el contexto («esto se
grabó sobre `abc1234`, estás en `def5678`») es v1.1. Es una limitación inherente que se
documenta, no se esconde.

## Riesgos

| Riesgo | Mitigación |
|--------|------------|
| **Secretos en las salidas de herramientas** | Un `Bash` que imprimió un `.env` acaba en un `.txt` que se publicaría sin mirar. No hay escáner: `remote-plan` devuelve la lista exacta de ficheros y la UI la muestra en el diálogo de confirmación. Revisarla es manual |
| Divergencia de código | Aviso al traer (arriba) |
| Payload grande | gzip; el ratio típico de un transcript ronda 3-4:1 |
| El repositorio como única copia | Si sustituye al histórico local (que los agentes purgan), pasa a ser infraestructura crítica y necesita backup |

## Límites conocidos de la v1

- **Republicar sobrescribe, no bifurca.** En el caso lineal (traes, continúas y
  republicas) no se pierde nada: tu transcript contiene la historia del otro más tu
  continuación. Pero si dos personas continúan la misma sesión en paralelo, la segunda
  publicación pisa a la primera en el repositorio. El campo `forked_from` ya viaja en el
  manifest; el fork explícito (`--fork-session`) queda para la siguiente.
- **Sin fusión de turnos.** Una fila `↻` avisa y abre tu copia: traer solo los turnos
  que faltan es un merge que todavía no existe.
- **Lo publicado no entra en la búsqueda de contenido (FTS)** hasta que se trae a disco.
- **La app nativa aún no tiene UI.** El motor está en el sidecar que ambas usan, así que
  añadirla es trabajo de panel, no de diseño.
- **Sin cifrado, sin presencia, sin publicación automática al cerrar.** Publicar es un
  acto consciente, deliberadamente.

## Cómo probarlo

Con una carpeta, sin configurar nada:

```bash
mkdir -p /tmp/repo-sesiones
export ATERM_REMOTE_DIR=/tmp/repo-sesiones

# la lista exacta de lo que subiría
agent-sessions-cli remote-plan claude <id>
# publicar, listar, traer a otro proyecto, despublicar
agent-sessions-cli remote-publish "$PWD" equipo claude <id>
agent-sessions-cli remote-list "$PWD" equipo
agent-sessions-cli remote-fetch "$PWD" equipo <id>
agent-sessions-cli remote-unpublish "$PWD" equipo <id>
```

El repositorio queda inspeccionable a mano, que es parte de la gracia de este backend:

```bash
find /tmp/repo-sesiones -type f
jq . /tmp/repo-sesiones/manifest/<id>.json
gzip -dc /tmp/repo-sesiones/blobs/<id>/session.jsonl.gz | head
```

Con GitLab/GitHub: crea un repositorio privado vacío, define el servidor
(`remote-server-set`, o Ajustes → «Servidores de sesiones compartidas…» en la
extensión), compruébalo con `remote-probe` y enlázalo al proyecto. El token necesita
lectura y escritura sobre ese repositorio (`api` en GitLab, `contents:write` en GitHub).

## Tests

- `tests/roundtrip.rs` — publicar → listar → traer → despublicar sobre una carpeta, con
  los bytes comparados; `memory/` y `session-env` verificadamente ausentes; manifest
  corrupto o de versión futura que solo se oculta a sí mismo; publicación interrumpida
  que es invisible en vez de estar rota; proveedor no reanudable que lo dice antes de
  tocar el disco.
- `tests/http_api.rs` — los drivers contra un **servidor HTTP real** en localhost, que
  reproduce las dos asimetrías (400 de GitLab al POST sobre fichero existente, 409 de
  GitHub sin `sha`). Respuestas grabadas habrían pasado con drivers que hablan otro
  idioma por el cable.
- `git.rs` — publica y despublica contra un repositorio git de verdad (bare, en disco),
  que es donde se comprueba que el borrado de ambos directorios no rompe el staging.
- Unitarios de manifest, payload por proveedor, layout, normalización de remotos y
  permisos del token.
