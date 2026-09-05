# TODO

> **Where the compose definitions live.** `main` carries the loader but no
> compose files — `crates/core/containers/` holds only a `.gitkeep`, and
> `crates/core/config/default.yml` is absent, so a `cargo run` of the `core`
> binary on `main` fails at configuration load. The actual stack (`haproxy`,
> `core.yaml`, the `observability_*` files, the vendored `containers/config/`
> and `config/default.yml`) lives on the unmerged
> `wip/add-docker-definitions-and-configurations` branch. Items below that
> speak of services "in the stack" mean that branch.

## Resource path resolution: switch to `current_exe()` for deployed binaries

`config/default.yml`, `containers/*.yaml`, and bind-mount sources resolved in
`resolve_host_path` are currently anchored with `env!("CARGO_MANIFEST_DIR")`
(the `CONFIG` consts in `crates/core/src/main.rs` and
`crates/init_kanidm_apisix/src/main.rs`, and `find_docker_definitions` in
`crates/core/src/docker.rs`, whose compose dir is what `resolve_host_path`
resolves relative sources against). This is a
compile-time constant — it bakes the build machine's source checkout path
(e.g. `/home/nino/repositories/core/crates/core`) directly into the binary.

This is enough to make `cargo run`/`cargo test` behave the same regardless of
invocation CWD, which was the immediate problem. It is **not** enough for a
binary that runs somewhere other than its build tree — a `cargo install`,
a CI-built release artifact, or (most relevant here, given this crate
orchestrates Docker) a slim runtime image that only `COPY`s the compiled
binary. In all of those cases the baked-in path won't exist, and resource
loading will fail again.

### The fix, when packaging/deployment is actually built

Resolve resource paths at **runtime**, relative to the running executable,
instead of at compile time relative to the source tree:

```rust
fn resource_dir() -> std::path::PathBuf {
    std::env::current_exe()
        .expect("failed to get current executable path")
        .parent()
        .expect("executable has no parent directory")
        .to_path_buf()
}
```

Then anchor `config/default`, `containers/`, and bind-mount sources on
`resource_dir()` instead of `env!("CARGO_MANIFEST_DIR")`.

**Precondition:** this only works if `config/` and `containers/` are
physically placed next to the compiled binary at runtime, in *every* context
the binary runs in — plain `cargo run` puts the binary in `target/debug/` or
`target/release/`, not next to `crates/core/config/`, so a packaging step
(build script, `Dockerfile` `COPY`, release archive step, etc.) needs to
co-locate them before this works locally too. Do this alongside whatever
Docker packaging/deployment story gets built for this crate, not before.

## Docker module: compose-file coverage

Today the parser (`ServiceConfig`, `src/docker.rs`) understands only a
narrow slice of the Compose spec: `image`, `ports`, `networks`, `volumes`,
`environment`, `container_name`, `command` (string form only),
`user`, `depends_on`, `restart`. Anything else in a real-world
`docker-compose.yml` is silently ignored, or makes the parse/boot fail. The
groups below track what's needed to boot an arbitrary compose file correctly.
Symbols named below are in `src/docker.rs` unless stated otherwise; they are
given as names rather than line numbers so the references survive edits.

### Main functionalities

The minimum needed to boot most real compose files correctly. Entries marked
`[x]` are implemented; they are kept for the record rather than deleted.

- [x] **`depends_on` + startup ordering.** Was: every service waited on one
  shared `Barrier`, so there was no ordering. Now supports the list form and
  the long form with `condition: service_started | service_healthy |
  service_completed_successfully`, and boots in dependency order — `DependsOn`
  / `DependencyCondition`, `wait_for_dependency`. Landed in `acfab00`.
- [x] **`restart` policy.** Was: a hardcoded 5s crash-restart loop. Now
  resolved to the container's `RestartPolicy` (`no`, `always`,
  `on-failure[:max]`, `unless-stopped`) by `resolve_restart_policy` and applied
  to the `HostConfig` built in `boot_service`. Landed in `57b8094`.
- [x] **Malformed `command` quoting was swallowed.** Was: `shlex::split`
  returns `None` on an unterminated quote or trailing backslash, and
  `.unwrap_or_default()` turned that into an empty `Vec`. Docker treats an
  empty `Cmd` as unset, so the container silently ran the **image's default
  command** instead of the one declared. Now resolved at load time into
  `ServiceConfig.command_argv` by `resolve_command`, so a bad quote aborts
  startup with `DockerModuleError::InvalidCommand` before any container is
  created. Landed in `e8d491e`.
  Concrete failure it prevented: a busybox init service that runs
  `sh -c "chown -R 8443:8443 /data"` to hand a volume to the uid its dependent
  runs as, gated by `service_completed_successfully`. A typo in that quoting
  made busybox run its default `sh`, which hits EOF and **exits 0** (verified),
  satisfying the gate, so the dependent booted as that uid against an unchowned
  volume. Verified alongside: `"User":""` leaves `Config.User` at the image's
  value, so the `Some("")` `boot_service` still sends for `user` is harmless.
- **Parse `healthcheck` + gate a service's own readiness on it.**
  `wait_for_healthy` already consumes Docker health state for `depends_on:
  condition: service_healthy`, but `healthcheck` is not a field on
  `ServiceConfig` at all, so a container defined here never *has* a health
  state to read — its no-healthcheck fallback degrades to "running". Parse
  `healthcheck` (`test`, `interval`, `timeout`, `retries`, `start_period`,
  `disable`) and pass it to `bollard`. Separately, a service's own boot gate
  (the readiness loop in `boot_service`) still only checks `running`/terminal,
  so a container that starts and immediately crash-loops under
  `restart: unless-stopped` is reported as started. That is what let a broken
  Kafka broker pass startup and surface 60s later as a client timeout.
- [x] **`environment` map form.** Was: only the `- KEY=VALUE` list form was
  accepted (`ServiceConfig.environment: Option<Vec<String>>`), so a file using
  the mapping form failed to parse outright. Now both forms resolve into
  `ServiceConfig.env` via `resolve_environment` — the `Environment` enum carries
  `serde_yaml::Value` map values so `REPLICAS: 3` and `DEBUG: true` coerce to
  strings rather than failing the parse, a bare `KEY` (list) or null `KEY:`
  (map) inherits from the host and is dropped when unset, and a duplicated key
  collapses to its last occurrence, matching what Docker does with `Env`.
- [x] **`env_file`.** Was: not parsed at all, so a service keeping its variables
  in a separate file silently got none of them. Now both the string and list
  forms load via `resolve_environment`, resolved against the compose dir by
  `resolve_host_path`, and merge under `environment` with Compose precedence —
  files in the order listed, then inline `environment` on top. Line parsing
  lives in `parse_env_file_content`; `export KEY=…` prefixes, multi-line values,
  and interpolation inside env files are not supported.
- **Variable interpolation `${VAR}` / `${VAR:-default}`.** Compose interpolates
  host env + `.env` into the file before parsing. None of this exists today;
  many real files depend on it.
  Concrete case: the APISIX admin key. `init_kanidm_apisix` reads it from
  `secrets/APISIX_ADMIN_KEY` (`SecretStore::apisix_admin_key` in
  `init_kanidm_apisix/src/secrets.rs`) to sign its admin calls, and APISIX
  resolves the same key out of its own container environment. A compose
  definition for the gateway therefore has to
  hardcode it, but `secrets/` is gitignored and the real key cannot be
  committed, so the definition can only carry a placeholder and the two sides
  are kept in sync by hand. Interpolating from the host environment would let
  both read one source.
- **`build:`.** Support building an image from a `context` (+ `dockerfile`,
  `args`, `target`) when `image:` is absent. Currently only the `create_image`
  (pull) path in `boot_service` is implemented.
- **`entrypoint`.** Not parsed; needed alongside `command` to run most images
  correctly.
- **`command` / `entrypoint` list form.** Only the string (shell) form is
  parsed (`ServiceConfig.command: Option<String>`), split by `shlex::split` in
  `resolve_command`. Accept the list form too.
- **Use `container_name`.** It is parsed but discarded (`_container_name`,
  in `boot_service`); containers are always named after the service key, via
  `CreateContainerOptions.name`. Because the service key is also the DNS name
  on the network, a file whose `container_name` differs from its service key
  silently breaks every hostname that refers to it — including the container's
  own env vars. Honor it when present, fall back to the service key otherwise.
- **Full `ports` short syntax.** Handle `container`-only, `/udp` protocol, and
  port ranges (`8000-8010:8000-8010`). `host:container` and `ip:host:container`
  both work today (the `ports` loop in `boot_service`).
- **`ports` long syntax.** `target` / `published` / `protocol` / `mode`
  mapping form.
- **Top-level `name:` (project namespace).** Compose prefixes resources with a
  project name. The `raw_name.to_string()` / `net.to_string()` calls in
  `find_docker_definitions` copy the name through unchanged — decide on and
  apply a real naming/namespacing scheme.

### Nice to have

Common but not required for a basic boot.

- **`labels`** (map and list form) on services, networks, volumes.
- **`expose`** (documented container ports without host publishing).
- **`deploy` block:** `resources.limits/reservations` (cpus/memory),
  `replicas`, `restart_policy`.
- **Resource limits (v2 style):** `mem_limit`, `mem_reservation`, `cpus`,
  `cpu_shares`, `pids_limit`, `shm_size`.
- **`logging`** driver + options → `HostConfig.log_config`.
- **`working_dir`, `hostname`, `domainname`, `stop_signal`,
  `stop_grace_period`.**
- **`extra_hosts`** → `HostConfig.extra_hosts`.
- **`dns`, `dns_search`.**
- **`cap_add` / `cap_drop`, `privileged`, `security_opt`, `devices`,
  `sysctls`, `ulimits`, `read_only`, `init`, `tmpfs`.**
- **`network_mode`, `pid`, `ipc`, `userns_mode`.**
- **Network long form per service:** `aliases`, `ipv4_address`, `ipv6_address`
  (currently only a plain name list, built in `boot_service`). `aliases` would
  also give `container_name` a natural implementation.
- **Network top-level options:** `external`, `internal`, `attachable`, `ipam`,
  `enable_ipv6`, `labels`, `driver_opts` (only `driver` is read on
  `NetworkConfig`).
- **Volume top-level options:** `external`, `driver_opts`, `labels`, `name`
  (only `driver` is read on `VolumeConfig`).
- **Volume/mount long-syntax sub-options:** `volume.nocopy`,
  `bind.propagation` / `create_host_path`, `tmpfs.size` / `tmpfs.mode`
  (`resolve_long_volume` ignores these).
- **Top-level `secrets` and `configs`** (+ per-service references, file/env
  sources).
- **`profiles`** (only start services whose profile is active).
- **`platform`** per service (currently always `String::new()` in the
  `CreateContainerOptions` built by `boot_service`).
- **`pull_policy`** (`always` / `missing` / `never` / `build`) instead of the
  current always-pull-when-untagged behavior (`resolve_pull_tag`).

### Edge cases

Correctness / robustness gaps that bite on specific files. Entries marked `[x]`
are implemented; they are kept for the record rather than deleted.

- [x] **Multi-service dependency cycles** in `depends_on` — detected and
  reported via `validate_dependency_graph` and the
  `DockerModuleError::DependencyCycle` variant.
- [x] **`inspect_container(...).unwrap()`** in the readiness loop — no longer
  panics if the container exits/vanishes during startup; the `Err` propagates
  with `?` (the readiness loop in `boot_service`).
- [x] **`panic!` on create/start failure** — all three sites now propagate as
  `DockerModuleError` and let the caller decide; no `panic!` or `unwrap()`
  remains outside the test module.
- **Duplicate resource names across files.** Services/networks/volumes from all
  `containers/*.yaml` are pushed into flat `Vec`s by `find_docker_definitions`;
  collisions silently duplicate. Detect and error (or last-wins with a warning).
- **Ambiguous bind vs. named-volume sources.** `~` / relative binds are handled
  (`resolve_host_path`), but Windows-style paths and sources that are ambiguous
  between a named volume and a host path (`is_host_path`) need review.
- **Empty / partial compose files** (`services:` absent, `null` service body)
  and non-map YAML — the current parse assumes well-formed structure.
- **YAML `extends`, anchors/aliases, and merge keys (`<<`).** serde_yaml
  resolves anchors, but `extends` (cross-file/service inheritance) is not
  supported.
- **`version:` / obsolete top-level keys** should be accepted-and-ignored, not
  cause a parse error — `deny_unknown_fields` is not set today, so verify
  unknown keys stay non-fatal.
- **Shutdown signal / grace period** is hardcoded `SIGTERM` + 10s
  (`stop_and_cleanup_container`); should honor `stop_signal` /
  `stop_grace_period`.
- **UDP / range port readiness and cleanup** once ranges are supported.

## Beyond Docker Compose: orchestration features Compose doesn't cover

Things this orchestrator could own that `docker compose` itself does not do.
These are product ideas, not compose-spec gaps.

- **Desired-state reconciliation loop** (controller style): continuously drive
  actual container state toward the declared set, not just a one-shot boot —
  re-create drifted/removed containers, converge on config changes.
- **Hot reload / file watching** of `containers/*.yaml`: apply add/remove/update
  of services without restarting the orchestrator.
- **Dependency-aware rolling restarts / zero-downtime redeploys** when an image
  or config changes (Compose only does a naive recreate).
- **Automatic image-update detection** (poll registry digests, pull + roll),
  à la watchtower.
- **Health-driven restart with backoff / circuit breaking** on top of Docker's
  `RestartPolicy`, plus alerting when a service stays unhealthy or crash-loops.
- **Volume backup / restore / snapshot** hooks for stateful services.
- **Secret sourcing from a vault / KMS** and rotation, beyond file/env secrets.
- **Built-in service discovery / dynamic DNS or reverse-proxy registration**
  (tie-in with the `haproxy` service already in the stack — see the note at the
  top of this file for where that lives).
- **Metrics / observability of the orchestrator itself** (expose container
  up/health/restart counts; wire into the existing Tempo/Vector/Grafana stack).
- **Resource-usage monitoring + threshold alerts** per container.
- **Readiness probes richer than Docker healthchecks** (e.g. TCP/HTTP probe a
  service, with dependency-condition timeouts, before marking deps ready).
- **Rollback on failed deploy** (keep last-known-good, revert on boot failure).

## Actual containers spinned up

- **Add Signoz**
- **Add OpenObserve Plugin to Grafana**