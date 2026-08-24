# TODO

## Resource path resolution: switch to `current_exe()` for deployed binaries

`config/default.yml`, `containers/*.yaml`, and bind-mount sources resolved in
`resolve_bind_source` are currently anchored with `env!("CARGO_MANIFEST_DIR")`
(see `src/configuration/mod.rs` and `src/docker/mod.rs`). This is a
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

Today the parser (`ServiceConfig`, `src/docker/mod.rs`) understands only a
narrow slice of the Compose spec: `image`, `ports`, `networks`, `volumes`,
`environment` (list form only), `container_name`, `command` (list form only),
`user`. Anything else in a real-world `docker-compose.yml` is silently ignored,
or makes the parse/boot fail. The groups below track what's needed to boot an
arbitrary compose file correctly. Line references are to `src/docker/mod.rs`.

### Main functionalities

The minimum needed to boot most real compose files correctly.

- **`depends_on` + startup ordering.** Today every service waits on one shared
  `Barrier` (`mod.rs:477`), so there is no ordering. Support the list form and
  the long form with `condition: service_started | service_healthy |
  service_completed_successfully`, and boot in dependency order.
- **`healthcheck` + real "healthy" gate.** The boot loop only checks
  `state.running` (`mod.rs:740-751`) despite logging "healthy". Parse
  `healthcheck` (`test`, `interval`, `timeout`, `retries`, `start_period`,
  `disable`), pass it to `bollard`, and gate readiness / `depends_on` on the
  actual health state.
- **`restart` policy.** Replace the hardcoded 5s crash-restart loop
  (`mod.rs:527-560`) with the container's `RestartPolicy` (`no`, `always`,
  `on-failure[:max]`, `unless-stopped`) via `HostConfig`.
- **`environment` map form.** Only the `- KEY=VALUE` list form is accepted
  (`ServiceConfig.environment: Option<Vec<String>>`). Also accept the
  `KEY: value` mapping form (and bare `KEY` → inherit from host).
- **`env_file`.** Load one or many env files (string / list) relative to the
  compose dir and merge under `environment`.
- **Variable interpolation `${VAR}` / `${VAR:-default}`.** Compose interpolates
  host env + `.env` into the file before parsing. None of this exists today;
  many real files depend on it.
- **`build:`.** Support building an image from a `context` (+ `dockerfile`,
  `args`, `target`) when `image:` is absent. Currently only `create_image`
  (pull) is implemented (`mod.rs:664-703`).
- **`entrypoint`.** Not parsed; needed alongside `command` to run most images
  correctly.
- **`command` / `entrypoint` string (shell) form.** Only the list form is
  parsed (`command: Option<Vec<String>>`). Accept the string form too, and
  don't send an empty `cmd`/`user` that overrides the image default
  (`mod.rs:643-656` currently sends `Some(vec![])` / `Some("")`).
- **Use `container_name`.** It is parsed but discarded (`_container_name`,
  `mod.rs:641`); containers are always named after the service key. Honor it
  when present, fall back to the service key otherwise.
- **Full `ports` short syntax.** Handle `container`-only, `host:container`,
  `ip:host:container`, `/udp` protocol, and port ranges
  (`8000-8010:8000-8010`). Today only exactly `host:container` TCP works
  (`mod.rs:612-630`).
- **`ports` long syntax.** `target` / `published` / `protocol` / `mode`
  mapping form.
- **Top-level `name:` (project namespace).** Compose prefixes resources with a
  project name. The `format!("{}", raw_name)` calls (`mod.rs:304,323,330`) are
  no-ops — decide on and apply a real naming/namespacing scheme.

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
  (currently only a plain name list, `mod.rs:632-637`).
- **Network top-level options:** `external`, `internal`, `attachable`, `ipam`,
  `enable_ipv6`, `labels`, `driver_opts` (only `driver` is read,
  `NetworkConfig`, `mod.rs:70-73`).
- **Volume top-level options:** `external`, `driver_opts`, `labels`, `name`
  (only `driver` is read, `VolumeConfig`, `mod.rs:75-78`).
- **Volume/mount long-syntax sub-options:** `volume.nocopy`,
  `bind.propagation` / `create_host_path`, `tmpfs.size` / `tmpfs.mode`
  (`resolve_long_volume` ignores these, `mod.rs:218-257`).
- **Top-level `secrets` and `configs`** (+ per-service references, file/env
  sources).
- **`profiles`** (only start services whose profile is active).
- **`platform`** per service (currently always `String::new()`, `mod.rs:661`).
- **`pull_policy`** (`always` / `missing` / `never` / `build`) instead of the
  current always-pull-when-untagged behavior (`resolve_pull_tag`,
  `mod.rs:99-110`).

### Edge cases

Correctness / robustness gaps that bite on specific files.

- **Duplicate resource names across files.** Services/networks/volumes from all
  `containers/*.yaml` are pushed into flat `Vec`s (`mod.rs:302-333`);
  collisions silently duplicate. Detect and error (or last-wins with a warning).
- **Multi-service dependency cycles** in `depends_on` — detect and fail clearly.
- **`inspect_container(...).unwrap()`** in the readiness loop (`mod.rs:743`)
  panics if the container exits/vanishes during startup; handle the `Err`.
- **`panic!` on create/start failure** (`mod.rs:698,722,762`) tears down the
  whole process; propagate as `DockerModuleError` and let the caller decide.
- **Image reference parsing.** `resolve_pull_tag` (`mod.rs:99-110`) mishandles
  registries with a port (`registry:5000/img`) and digests (`img@sha256:...`);
  a registry-with-port has a `:` in a non-tag segment.
- **Ambiguous bind vs. named-volume sources.** `~` / relative binds are handled
  (`resolve_host_path`), but Windows-style paths and sources that are ambiguous
  between a named volume and a host path (`is_host_path`, `mod.rs:112-118`)
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
  (`stop_and_cleanup_container`, `mod.rs:447-450`); should honor `stop_signal`
  / `stop_grace_period`.
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
- **Health-driven restart with backoff / circuit breaking** instead of the
  fixed 5s loop, plus alerting when a service stays unhealthy.
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
