# TODO

> **Where the compose definitions live.** `main` carries the compose runner but
> no compose files — `crates/core/containers/` holds only a `.gitkeep`, and
> `crates/core/config/default.yml` is absent, so a `cargo run` of the `core`
> binary on `main` fails at configuration load. The actual stack (`haproxy`,
> `core.yaml`, the `observability_*` files, the vendored `containers/config/`
> and `config/default.yml`) lives on the unmerged
> `wip/add-docker-definitions-and-configurations` branch. Items below that
> speak of services "in the stack" mean that branch.

## Compose runner

Symbols named in this section are in `crates/core/src/compose.rs` unless
stated otherwise.

`core` no longer parses compose files itself. `start_compose_stack` hands every
`*.yaml` / `*.yml` directly inside `containers/` (found by
`find_compose_files`, sorted, not recursive) to `docker compose up --detach
--wait` as one project named `PROJECT_NAME` (`ninoverse`), and registers a task
that runs `docker compose down` — or `stop`, when
`docker.remove_containers_on_shutdown` is false — once shutdown is broadcast.
Compose's own output goes straight to the terminal, not through `logger`.
Docker Compose v2 (the `docker compose` plugin) is a runtime prerequisite.

### Why the hand-rolled loader was dropped

The previous `src/docker.rs` (~2,300 lines) parsed compose files into its own
`ServiceConfig` and drove `bollard` directly. It covered twelve compose keys and
silently ignored the rest, so a valid compose file meant something different
here than under `docker compose`. That cost real bugs: a malformed `command`
quote ran the image's default command, the `environment` map form failed to
parse, `entrypoint` was ignored, `container_name` was discarded, and
`healthcheck` was never parsed — so `condition: service_healthy` degraded to
"running". The remaining gap list (`build:`, full `ports` syntax, `labels`,
`deploy`, resource limits, `logging`, security options, network/volume long
forms, `secrets`/`configs`, `profiles`, `platform`, `pull_policy`, `extends`,
`stop_signal`/`stop_grace_period`, …) was the whole rest of the spec. Compose
already implements all of it, and nothing the module added on top was
something Compose lacks. The orchestration ideas that do go beyond Compose
(below) only need the Docker API, not ownership of the parser.

### Before merging the wip branch

Checked by running `docker compose config` over that branch's
`containers/*.yaml`. Everything else merges and validates as one project.

- **Two files make Compose reject the whole project.** `sonarqube.yaml` is
  commented out entirely (`empty compose file`) and `observability_generator.yaml`
  keeps a bare `services:` with every entry commented out (`services must be a
  mapping`). Delete them, move them out of `containers/`, or rename them to a
  non-`.yaml` extension.
- **Named volumes change name.** Compose prefixes them with the project name, so
  `etcd-data` becomes `ninoverse_etcd-data`, and so on. Volumes created by the old
  loader hold existing data (Postgres, kanidm, Grafana, …) and will **not** be
  reused. Either set `name: <old-name>` on each top-level volume to keep
  using them, or copy the data across once.
- **Networks change name too.** `ninoverse-network` becomes
  `ninoverse_ninoverse-network`. Service-name DNS is unaffected; set
  `name: ninoverse-network` if anything outside the project refers to it by
  name. `etcd-backend`'s `internal: true`, ignored by the old loader, is now
  honored.
- **APISIX admin key.** `containers/api_gateway.yaml` still hardcodes `ADMIN_KEY`.
  `init_kanidm_apisix` reads the key from `secrets/APISIX_ADMIN_KEY`
  (`SecretStore::apisix_admin_key` in `init_kanidm_apisix/src/secrets.rs`).
  Switch the compose file to `${APISIX_ADMIN_KEY:?…}` with the value in
  `containers/.env` to retire the by-hand sync. Use the `:?` form: Compose
  substitutes an empty string for a plain `${VAR}` that is unset, where the old
  loader failed.

### Open

- **A crash-looping container without a healthcheck passes `--wait`.** Verified:
  a service that exits non-zero under `restart: unless-stopped` is reported
  healthy and `up` exits 0. With a `healthcheck`, `up --wait` fails as it should.
  This is what let a broken Kafka broker pass startup and surface 60s later as a
  client timeout. Fix it in the compose files: give every long-running service
  that others depend on a `healthcheck`, and use `condition: service_healthy`.
- **`remove_volumes_on_shutdown` and `remove_networks_on_shutdown`** are fields of
  `DockerConfiguration` (`crates/configuration`) that nothing reads, before or
  after this change. `down --volumes` would honor the first. The second cannot
  be honored as `false` with `down`, which always removes the project's
  networks. Either wire them or remove them.
- **A service key repeated across files is merged, not reported.** Compose treats
  later `--file`s as overrides, so two files declaring the same service combine
  their fields. The old loader created duplicates instead. Neither reports it.
  `find_compose_files` sorts by name, so the merge order is stable.

## Resource path resolution: switch to `current_exe()` for deployed binaries

`config/default.yml` and `containers/` are currently anchored with
`env!("CARGO_MANIFEST_DIR")` (the `CONFIG` consts in `crates/core/src/main.rs`
and `crates/init_kanidm_apisix/src/main.rs`, and `COMPOSE_DIR` in
`crates/core/src/compose.rs`, which is also the `--project-directory` relative
bind-mount sources resolve against). This is a compile-time constant — it bakes
the build machine's source checkout path (e.g.
`/home/nino/repositories/core/crates/core`) directly into the binary.

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

Then anchor `config/default` and `containers/` on `resource_dir()` instead of
`env!("CARGO_MANIFEST_DIR")`.

**Precondition:** this only works if `config/` and `containers/` are
physically placed next to the compiled binary at runtime, in *every* context
the binary runs in — plain `cargo run` puts the binary in `target/debug/` or
`target/release/`, not next to `crates/core/config/`, so a packaging step
(build script, `Dockerfile` `COPY`, release archive step, etc.) needs to
co-locate them before this works locally too. Do this alongside whatever
Docker packaging/deployment story gets built for this crate, not before.

## Beyond Docker Compose: orchestration features Compose doesn't cover

Things this orchestrator could own that `docker compose` itself does not do.
These are product ideas, not compose-spec gaps. They build on top of the
containers Compose creates: the Docker API (`bollard`) can inspect them and
subscribe to their events, filtered on the `com.docker.compose.project=ninoverse`
label, without `core` owning the compose parser.

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
