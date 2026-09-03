# TODO

## Resource path resolution: switch to `current_exe()` for deployed binaries

`config/default.yml`, `containers/*.yaml`, and bind-mount sources resolved in
`resolve_host_path` are currently anchored with `env!("CARGO_MANIFEST_DIR")`
(`crates/core/src/main.rs:32`, `crates/core/src/docker.rs:391`, and
`crates/init_kanidm_apisix/src/main.rs:9`). This is a
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
`environment` (list form only), `container_name`, `command` (string form only),
`user`, `depends_on`, `restart`. Anything else in a real-world
`docker-compose.yml` is silently ignored, or makes the parse/boot fail. The
groups below track what's needed to boot an arbitrary compose file correctly.
Line references are to `src/docker.rs`.

### Main functionalities

The minimum needed to boot most real compose files correctly. Entries marked
`[x]` are implemented; they are kept for the record rather than deleted.

- [x] **`depends_on` + startup ordering.** Was: every service waited on one
  shared `Barrier`, so there was no ordering. Now supports the list form and
  the long form with `condition: service_started | service_healthy |
  service_completed_successfully`, and boots in dependency order — `DependsOn`
  / `DependencyCondition` (`docker.rs:82-108`), `wait_for_dependency`
  (`docker.rs:681-704`). Landed in `acfab00`.
- [x] **`restart` policy.** Was: a hardcoded 5s crash-restart loop. Now
  resolved to the container's `RestartPolicy` (`no`, `always`,
  `on-failure[:max]`, `unless-stopped`) and applied via `HostConfig` —
  `resolve_restart_policy` (`docker.rs:189-215`), applied at `docker.rs:936`.
  Landed in `57b8094`.
- **Parse `healthcheck` + gate a service's own readiness on it.**
  `wait_for_healthy` (`docker.rs:706-732`) already consumes Docker health state
  for `depends_on: condition: service_healthy`, but `healthcheck` is not a
  field on `ServiceConfig` at all, so a container defined here never *has* a
  health state to read — the fallback at `docker.rs:720-725` silently degrades
  to "running". Parse `healthcheck` (`test`, `interval`, `timeout`, `retries`,
  `start_period`, `disable`) and pass it to `bollard`. Separately, a service's
  own boot gate (`docker.rs:1087-1116`) still only checks `running`/terminal,
  so a container that starts and immediately crash-loops under
  `restart: unless-stopped` is reported as started. That is what let a broken
  Kafka broker pass startup and surface 60s later as a client timeout.
- **`environment` map form.** Only the `- KEY=VALUE` list form is accepted
  (`ServiceConfig.environment: Option<Vec<String>>`). Also accept the
  `KEY: value` mapping form (and bare `KEY` → inherit from host).
- **`env_file`.** Load one or many env files (string / list) relative to the
  compose dir and merge under `environment`.
- **Variable interpolation `${VAR}` / `${VAR:-default}`.** Compose interpolates
  host env + `.env` into the file before parsing. None of this exists today;
  many real files depend on it.
  Concrete case: the APISIX admin key. `init_kanidm_apisix` reads it from
  `secrets/APISIX_ADMIN_KEY` (`init_kanidm_apisix/src/secrets.rs:84-100`) to
  sign its admin calls, and APISIX resolves the same key out of its own
  container environment. A compose definition for the gateway therefore has to
  hardcode it, but `secrets/` is gitignored and the real key cannot be
  committed, so the definition can only carry a placeholder and the two sides
  are kept in sync by hand. Interpolating from the host environment would let
  both read one source.
- **`build:`.** Support building an image from a `context` (+ `dockerfile`,
  `args`, `target`) when `image:` is absent. Currently only `create_image`
  (pull) is implemented (`docker.rs:1003-1050`).
- **`entrypoint`.** Not parsed; needed alongside `command` to run most images
  correctly.
- **`command` / `entrypoint` list form.** Only the string (shell) form is
  parsed, via `shlex::split` (`command: Option<String>`, `docker.rs:982`).
  Accept the list form too.
- **Malformed `command` quoting is swallowed.** `shlex::split` returns `None`
  on an unterminated quote or trailing backslash, and `.unwrap_or_default()`
  (`docker.rs:982`) turns that into an empty `Vec`. Docker treats an empty
  `Cmd` as unset, so the container silently runs the **image's default
  command** instead of the one declared. Verified: creating a container with
  `"Cmd":[]` leaves `Config.Cmd` at the image value (`["postgres"]`,
  `["sh"]`); `"User":""` likewise leaves `Config.User` at the image's `472`.
  So `docker.rs:993-994` sending `Some(vec![])` / `Some("")` is harmless — the
  real bug is the discarded parse error. Per `.claude/code-review.md` this
  should surface as a `DockerModuleError`, not a default.
  Concrete failure: a busybox init service that runs
  `sh -c "chown -R 8443:8443 /data"` to hand a volume to the uid its dependent
  runs as, gated by `service_completed_successfully`. A typo in that quoting
  makes busybox run its default `sh`, which hits EOF and **exits 0** —
  verified — satisfying the gate, so the dependent boots as that uid against an
  unchowned volume.
- **Use `container_name`.** It is parsed but discarded (`_container_name`,
  `docker.rs:980`); containers are always named after the service key
  (`docker.rs:999`). Because the service key is also the container's DNS name
  on the network, a file whose `container_name` differs from its service key
  silently breaks every hostname that refers to it — including the container's
  own env vars. Honor it when present, fall back to the service key otherwise.
- **Full `ports` short syntax.** Handle `container`-only, `/udp` protocol, and
  port ranges (`8000-8010:8000-8010`). `host:container` and `ip:host:container`
  both work today (`docker.rs:950-969`).
- **`ports` long syntax.** `target` / `published` / `protocol` / `mode`
  mapping form.
- **Top-level `name:` (project namespace).** Compose prefixes resources with a
  project name. The `raw_name.to_string()` / `net.to_string()` calls
  (`docker.rs:408,411,429,436`) copy the name through unchanged — decide on and
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
  (currently only a plain name list, `docker.rs:971-976`). `aliases` would also
  give `container_name` a natural implementation.
- **Network top-level options:** `external`, `internal`, `attachable`, `ipam`,
  `enable_ipv6`, `labels`, `driver_opts` (only `driver` is read,
  `NetworkConfig`, `docker.rs:134-137`).
- **Volume top-level options:** `external`, `driver_opts`, `labels`, `name`
  (only `driver` is read, `VolumeConfig`, `docker.rs:139-142`).
- **Volume/mount long-syntax sub-options:** `volume.nocopy`,
  `bind.propagation` / `create_host_path`, `tmpfs.size` / `tmpfs.mode`
  (`resolve_long_volume` ignores these, `docker.rs:323-363`).
- **Top-level `secrets` and `configs`** (+ per-service references, file/env
  sources).
- **`profiles`** (only start services whose profile is active).
- **`platform`** per service (currently always `String::new()`,
  `docker.rs:1000`).
- **`pull_policy`** (`always` / `missing` / `never` / `build`) instead of the
  current always-pull-when-untagged behavior (`resolve_pull_tag`,
  `docker.rs:174-187`).

### Edge cases

Correctness / robustness gaps that bite on specific files. Entries marked `[x]`
are implemented; they are kept for the record rather than deleted.

- [x] **Multi-service dependency cycles** in `depends_on` — detected and
  reported via `validate_dependency_graph` (`docker.rs:612-679`) and the
  `DockerModuleError::DependencyCycle` variant (`docker.rs:167-168`).
- [x] **`inspect_container(...).unwrap()`** in the readiness loop — no longer
  panics if the container exits/vanishes during startup; the `Err` propagates
  with `?` (`docker.rs:1088-1090`).
- [x] **`panic!` on create/start failure** — all three sites now propagate as
  `DockerModuleError` and let the caller decide; no `panic!` or `unwrap()`
  remains outside the test module.
- **Duplicate resource names across files.** Services/networks/volumes from all
  `containers/*.yaml` are pushed into flat `Vec`s (`docker.rs:407-440`);
  collisions silently duplicate. Detect and error (or last-wins with a warning).
- **Image reference parsing.** `resolve_pull_tag` (`docker.rs:174-187`)
  mishandles registries with a port (`registry:5000/img`) and digests
  (`img@sha256:...`); a registry-with-port has a `:` in a non-tag segment.
- **Ambiguous bind vs. named-volume sources.** `~` / relative binds are handled
  (`resolve_host_path`), but Windows-style paths and sources that are ambiguous
  between a named volume and a host path (`is_host_path`, `docker.rs:217-223`)
  need review.
- **Empty / partial compose files** (`services:` absent, `null` service body)
  and non-map YAML — the current parse assumes well-formed structure.
- **YAML `extends`, anchors/aliases, and merge keys (`<<`).** serde_yaml
  resolves anchors, but `extends` (cross-file/service inheritance) is not
  supported.
- **`version:` / obsolete top-level keys** should be accepted-and-ignored, not
  cause a parse error — `deny_unknown_fields` is not set today, so verify
  unknown keys stay non-fatal.
- **Shutdown signal / grace period** is hardcoded `SIGTERM` + 10s
  (`stop_and_cleanup_container`, `docker.rs:554-557`); should honor
  `stop_signal` / `stop_grace_period`.
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
  (tie-in with the `haproxy` service already in the stack).
- **Metrics / observability of the orchestrator itself** (expose container
  up/health/restart counts; wire into the existing Tempo/Vector/Grafana stack).
- **Resource-usage monitoring + threshold alerts** per container.
- **Readiness probes richer than Docker healthchecks** (e.g. TCP/HTTP probe a
  service, with dependency-condition timeouts, before marking deps ready).
- **Rollback on failed deploy** (keep last-known-good, revert on boot failure).

## Actual containers spinned up

- **Add Signoz**
- **Add OpenObserve Plugin to Grafana**