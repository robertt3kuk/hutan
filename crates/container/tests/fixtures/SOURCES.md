# Fixtures and where they come from

No container runtime is installed on the development machine and none was started to make these
files. Each one is transcribed from a vendor document, vendor source, or a `docker info` /
`docker version` listing that a user posted in a public issue. **Synthetic** marks values that no
source gave us; those fixtures exist to exercise one identification rule and say which.
`*.http` files are raw HTTP responses with `\n` line ends (the tests turn them into CRLF);
header names are spelled the way Go's `net/http` canonicalises them.

Personal values in the listings (engine IDs, host names, user names) were dropped or replaced
(`/Users/me`); container and image counts are omitted because detection never reads them.

## Apple `container` — `apple/`

| File | Source |
|---|---|
| `status_running.json`, `status_not_running.json`, `status_unregistered.json` | The `StatusPayload` / `ClientInfo` / `ServerInfo` / `HostInfo` / `PathInfo` / `ResourceCounts` `Codable` structs and the three `status` strings of [`Sources/ContainerCommands/System/SystemStatus.swift`](https://github.com/apple/container/blob/main/Sources/ContainerCommands/System/SystemStatus.swift) (`"unregistered"`, `"not running"`, `"running"`; nil members are omitted by `Codable`). Values are illustrative: version 1.6.1 is the release the research read; the long `server.version` form follows the `system version` example below; the commit is `unspecified`, the string the source uses when none is compiled in. |
| `status_unknown_state.json` | Synthetic: a `status` string the source does not define, to prove the parser refuses what it does not know. |
| `system_version_with_server.json` | Verbatim JSON example of `container system version --format json` in [`docs/command-reference.md`](https://github.com/apple/container/blob/main/docs/command-reference.md#container-system-version). |
| `system_version_client_only_1_6_1.json` | Same shape; the reference says the apiserver entry is added only when the server answers a health check, so with the services down only the CLI entry is printed. Version edited to 1.6.1. |

## Docker Engine family — `docker/`

Key names and types: Engine API [`swagger.yaml`](https://github.com/moby/moby/blob/master/api/swagger.yaml) `SystemVersion`, `SystemInfo` (only `Name`, `ServerVersion`, `OperatingSystem`, `OSType`, `Architecture`, `KernelVersion` are kept). `/_ping` headers: `Server`, `Api-Version`, `Ostype` from [`daemon/server/middleware/version.go`](https://github.com/moby/moby/blob/master/daemon/server/middleware/version.go), `Builder-Version`, `Swarm`, `Cache-Control`, `Pragma` and the `OK` body from [`daemon/server/router/system/system_routes.go`](https://github.com/moby/moby/blob/master/daemon/server/router/system/system_routes.go).

| Directory | Values from |
|---|---|
| `docker_desktop/` | The `docker version` and `docker info` listing of Docker Desktop 4.67.0 in [docker/desktop-feedback#270](https://github.com/docker/desktop-feedback/issues/270): engine 29.3.1, API 1.54 (minimum 1.40), `Server: Docker Desktop 4.67.0 (222858)` → `Platform.Name`, `Name: docker-desktop`, `Operating System: Docker Desktop`, kernel `6.12.76-linuxkit`. |
| `colima/` | The listing in [testcontainers/testcontainers-go#2952](https://github.com/testcontainers/testcontainers-go/issues/2952) (context `colima`): engine 26.1.1, API 1.45 (minimum 1.24), `Name: colima`, Ubuntu 24.04 LTS, kernel `6.8.0-31-generic`; socket `~/.colima/default/docker.sock` ([Colima FAQ](https://colima.run/docs/faq/#where-is-the-docker-socket-located)). |
| `orbstack/` | The `docker info` of OrbStack posted in [kubernetes-sigs/kind#3171](https://github.com/kubernetes-sigs/kind/issues/3171) (context `orbstack`): server 23.0.3, `Name: docker`, `Operating System: Alpine Linux edge (containerized)`, kernel `6.1.23-orbstack-00113-g6614aaccb205-dirty` (the same `-orbstack-` kernel appears in OrbStack's own boot log, orbstack/orbstack#484). API 1.42 / minimum 1.12 are Engine 23.0's documented values. The listing has no `docker version`, so `version.json` carries only the fields above. |
| `rancher_desktop/` | **Synthetic.** Version, API, kernel and the rest are the swagger example values (27.0.1, 1.47, `6.8.0-31-generic`). Only `Name: lima-rancher-desktop` is vendor-sourced: the VM hostname set in Rancher Desktop's [`lima-config.yaml`](https://github.com/rancher-sandbox/rancher-desktop-2/blob/main/pkg/rancher-desktop/assets/lima-config.yaml). No `docker info` of Rancher Desktop was found. |
| `lima/` | **Synthetic**, swagger example values, `Name: lima-docker`: stands for any engine none of the rules recognise (Lima's docker template, a plain `dockerd`). |
| `not_an_engine/` | Synthetic: what Go's `http.NotFound` prints (`404 page not found`), i.e. a unix socket that speaks HTTP but is not an Engine. |

## Podman — `podman/`

| File | Source |
|---|---|
| `machine_inspect_running.json` | The example of [`podman-machine-inspect(1)`](https://docs.podman.io/en/latest/markdown/podman-machine-inspect.1.html) (`Name`, `State: running`, `ConnectionInfo.PodmanSocket.Path`, …), made a JSON array as the page shows. |
| `machine_inspect_stopped.json` | Same, `State: stopped`. |
| `ping.http` | The headers set by podman's [`pkg/api/handlers/compat/ping.go`](https://github.com/containers/podman/blob/main/pkg/api/handlers/compat/ping.go) and [`pkg/api/server/handler_api.go`](https://github.com/containers/podman/blob/main/pkg/api/server/handler_api.go) (`Server: Libpod/<version> (<os>)`, `Libpod-API-Version`, `API-Version`, `Builder-Version: 1`, `Docker-Experimental`, `OSType`). Version numbers are synthetic (5.6.2). |
| `version.json` | The structure built by [`pkg/api/handlers/compat/version.go`](https://github.com/containers/podman/blob/main/pkg/api/handlers/compat/version.go): components `Podman Engine`, `Conmon`, `OCI Runtime (<name>)`; `Platform.Name` is `<os>/<arch>/<distribution>-<version>`. Values are synthetic. |
