# Adding SigNoz to the stack

**Status: proposal. Nothing described here is deployed.** The only trace of SigNoz
in the tree today is the bare `# signoz:` placeholder at
[observability_ui.yaml:14](../crates/core/containers/observability_ui.yaml#L14).

> **Note:** every `crates/core/containers/…` link below resolves only on the
> `wip/add-docker-definitions-and-configurations` branch. On `main` that
> directory holds just a `.gitkeep`; the compose definitions are not merged yet.

This document records what SigNoz actually needs, what the repository already
provides that it can reuse, and the loader gaps that have to close before a
working definition can be written.

---

## 1. Provenance: upstream no longer ships Compose

SigNoz's `deploy/` directory now contains only a deprecation notice. Installation
moved to **Foundry** (`foundryctl forge` / `foundryctl cast`, driven by a
`casting.yaml`), and upstream states plainly: *"SigNoz no longer distributes these
files"*.

The last release that shipped a `docker-compose.yaml` is **v0.129.0**; from
v0.130.0 onward the file is gone (verified: `deploy/docker/docker-compose.yaml`
returns 404 at v0.130.0+, 200 at v0.129.0).

So any definition added here is a port of a **frozen** upstream file:

```
https://raw.githubusercontent.com/SigNoz/signoz/v0.129.0/deploy/docker/docker-compose.yaml
```

Pinned versions from that file:

| Component | Image |
|---|---|
| SigNoz UI + API | `signoz/signoz:v0.129.0` |
| Collector / migrator | `signoz/signoz-otel-collector:v0.144.5` |
| ClickHouse | `clickhouse/clickhouse-server:25.5.6` |
| ZooKeeper | `signoz/zookeeper:3.7.1` |

The current SigNoz release line is well past this (v0.138.x at time of writing).
Tracking newer versions means either following Foundry's generated manifests or
maintaining the port by hand. That tradeoff should be decided before adopting
SigNoz as more than an evaluation backend, since the rest of
[observability_databases.yaml](../crates/core/containers/observability_databases.yaml)
is exactly that — backends kept side by side for comparison.

---

## 2. What SigNoz requires

Six services, not one. The `# signoz:` placeholder sitting in
`observability_ui.yaml` next to Grafana is misleading — SigNoz is not a UI you
point at an existing datastore. It brings its own ClickHouse.

| Service | Role |
|---|---|
| `init-clickhouse` | **One-shot.** Downloads the `histogramQuantile` binary into a `user_scripts` dir shared with ClickHouse. Backs the `histogramQuantile` UDF used for histogram percentile queries. |
| `zookeeper-1` | Coordination for ClickHouse replicated tables. |
| `clickhouse` | The telemetry store. Traces, metrics, logs, metadata. |
| `signoz` | UI + query API on `:8080`. SQLite metadata at `/var/lib/signoz/`. Also serves OpAMP on `:4320`. |
| `otel-collector` | OTLP receiver on `:4317`/`:4318`, writes to ClickHouse. |
| `signoz-telemetrystore-migrator` | **One-shot.** `migrate bootstrap && migrate sync up && migrate async up` — creates the ClickHouse schema. |

**The ordering contract matters.** Upstream expresses it with `depends_on` +
healthchecks:

- ClickHouse waits on `init-clickhouse` completing *and* `zookeeper-1` healthy.
- Migrator and `signoz` wait on ClickHouse healthy.
- The collector additionally gates itself on `/signoz-otel-collector migrate sync
  check` succeeding before it starts serving.

The `depends_on` half of that is expressible today; the healthcheck half is not,
so `condition: service_healthy` currently degrades to "is running" — see §4.

---

## 3. What can be reused as-is

This is the part that keeps a SigNoz port small.

### Network

`ninoverse-network` is declared once, in
[core.yaml](../crates/core/containers/core.yaml). SigNoz's own `signoz-net`
should simply be dropped; every SigNoz service joins `ninoverse-network` like
everything else. Docker's embedded DNS then resolves service names across the
whole stack, which is what HAProxy and Vector already rely on.

Do **not** copy `kanidm.yaml`'s `networks: ninoverse-network: {external: true}`
pattern — `NetworkConfig` only reads `driver`, so `external` is silently ignored
and the network is pushed onto the create list anyway. It works only because a
409 from Docker is swallowed. A new file should just not re-declare the network.

### Config file placement

Vendored ClickHouse XML belongs in `crates/core/containers/config/`, mounted
read-only, exactly like the four configs already there:

| File | Mounted by | Target |
|---|---|---|
| `config/apisix.yaml` | `apisix` | `/usr/local/apisix/conf/config.yaml:ro` |
| `config/haproxy.cfg` | `haproxy` | `/usr/local/etc/haproxy/haproxy.cfg:ro` |
| `config/tempo.yaml` | `tempo` | `/etc/tempo.yaml:ro` |
| `config/vector.yaml` | `vector` | `/etc/vector/vector.yaml:ro` |
| `config/kanidm/server.toml` | `kanidmd` | `/data/server.toml:ro` |

A `config/signoz/` subdirectory follows the `config/kanidm/` precedent.

> **The `./` prefix is mandatory.** `is_host_path` in
> [docker.rs](../crates/core/src/docker.rs) classifies a bind source
> by its prefix — `/`, `./`, `../`, `~`, `~/`. A source written as
> `config/signoz/cluster.xml` is treated as a *named volume literally called
> `config/signoz/cluster.xml`*, not a bind. This is precisely the bug the deleted
> `logs_signoz.yaml` had.

### Ingest: reuse the Vector fan-out

[config/vector.yaml](../crates/core/containers/config/vector.yaml) is already the
single ingest point. It receives on `otel_receiver` (gRPC `0.0.0.0:4317`, HTTP
`0.0.0.0:4318`) and fans out to five sinks: `loki_out`, `tempo_out`, `mimir_out`,
`victorialogs_out`, `openobserve_out` (with `elasticsearch_out` / `opensearch_out`
commented out).

A SigNoz sink slots in beside `tempo_out`, which is the closest analogue — same
type, same codec:

```yaml
  signoz_out:
    type: opentelemetry
    inputs: [otel_receiver.traces]
    protocol:
      type: "http"
      uri: "http://signoz-otel-collector:4318/v1/traces"
      encoding:
        codec: "otlp"
```

Consequences of routing through Vector rather than exposing SigNoz's collector
directly:

- SigNoz's collector needs **no published host ports** — nothing new to bind, no
  clash with the `otlp_grpc_in` frontend HAProxy already binds on `:4317`.
- The existing edge path (`otlp.nicolapasqualini.it` + `path_beg /v1/traces
  /v1/metrics /v1/logs` → `vector:4318`, and the TCP `:4317` frontend →
  `vector:4317`) keeps working untouched.
- SigNoz sees the same telemetry as every other backend, which is the point of an
  evaluation stack.

The alternative — pointing producers straight at `signoz-otel-collector:4317/4318`
— bypasses Vector and gives SigNoz raw OTLP the others don't see. Doing *both*
risks double-counting the same spans.

### Edge: reuse the HAProxy host routing

[config/haproxy.cfg](../crates/core/containers/config/haproxy.cfg) routes by Host
ACL in `frontend https_router`. Two options, both one-liners:

**Behind APISIX (recommended)** — append to the existing `is_apisix` ACL list,
beside `openobserve.` / `victorialogs.` / `sonarqube.`:

```
    acl is_apisix  var(txn.host) -m str signoz.nicolapasqualini.it
```

Repeating the ACL name ORs the entries, so this needs no new backend and no new
`use_backend` rule. SigNoz inherits the kanidm identity boundary that APISIX
supplies — which matters, because SigNoz's own auth is not configured here and
`SIGNOZ_TOKENIZER_JWT_SECRET=secret` in the upstream compose is a literal
placeholder.

**Dedicated backend** — mirror `grafana_backend`:

```
backend signoz_backend
    server signoz signoz:8080
```

with its own `acl is_signoz` / `use_backend signoz_backend if is_signoz`. This is
unauthenticated, the same as Grafana is today.

Either way the hostname must be added to the `certbot certonly --expand -d` SAN
list recorded in [TODO.md](../TODO.md), or TLS for `signoz.nicolapasqualini.it`
will fail.

### Conventions to match

- **Pin every image** to an explicit tag or digest. Never `latest` — the whole
  working tree was recently converted away from it, and `sftp.yaml` even pins by
  `sha256` digest with a comment explaining why.
- **Co-locate the `volumes:` block** in the same file as the services using it.
  The old central `volumes.yaml` was deliberately dissolved into per-file blocks.
- **The service key *is* the container name.** `container_name:` is parsed and
  then discarded by `boot_service`; the container is always named after the YAML
  key. Service keys are therefore a flat global namespace across *every* file in
  `containers/`. SigNoz's upstream keys
  (`clickhouse`, `zookeeper-1`, `otel-collector`) are far too generic for a shared
  network — prefix them: `signoz-clickhouse`, `signoz-zookeeper`,
  `signoz-otel-collector`.
- **A new file is picked up automatically.** `find_docker_definitions` globs
  every `*.yaml` and `*.yml` in `containers/` and flattens the three maps; file
  names carry no meaning and no Rust change is needed to register one.
  `containers/config/` is skipped because it is a directory, not a file.

Because SigNoz brings its own datastore and collector, it does **not** belong in
`observability_ui.yaml` next to Grafana. A dedicated `signoz.yaml` is the right
shape, and the `# signoz:` placeholder should be removed when it lands.

---

## 4. What blocks a faithful port

Symbols named in this section live in
[docker.rs](../crates/core/src/docker.rs); they are given as names rather than
line numbers so the references survive edits.

`ServiceConfig` parses exactly ten compose keys: `image`, `ports`, `networks`,
`volumes`, `environment`, `container_name`, `command`, `user`, `depends_on`,
`restart` (plus four `#[serde(skip)]` fields the loader fills in itself —
`mounts`, `env`, `restart_policy`, `command_argv`). No struct uses
`deny_unknown_fields`, so everything else is **silently dropped** — the failure is
invisible, not loud.

### No `entrypoint`

Both one-shot jobs rely on `entrypoint: /bin/sh` + `command: -c '<a && b>'`.
Here `command` is a single string run through `shlex::split` in
`resolve_command`, so `&&` arrives as a literal argument to the image's own
entrypoint rather than being interpreted by a shell.

- `init-clickhouse` can be worked around: swap the image for one with an empty
  ENTRYPOINT and use `command: 'sh -c "..."'` — the trick `kanidm.yaml`'s
  `init-volume` already uses with `busybox:1.38.0`.
- The **migrator cannot be**. Its image entrypoint is the collector binary, so
  `migrate bootstrap && migrate sync up && migrate async up` would have to be
  split across three separate containers with no way to order them.

Adding `entrypoint: Option<String>` to `ServiceConfig` and passing it through to
`ContainerCreateBody.entrypoint` is a small, contained change that removes this
entire class of workaround.

### No `healthcheck`, so `service_healthy` is not a real gate

`depends_on` itself works (see "Resolved since this was written" below), but
`healthcheck` is not a field on `ServiceConfig`, so a container this loader
creates never *has* a Docker health state. `wait_for_healthy` reads that state
for `condition: service_healthy` and, finding none, falls back to "the container
is running" — which is exactly the gate SigNoz's ordering contract needs to *not*
be.

That matters concretely here:

- ClickHouse is slow to accept connections after its process starts. `signoz` and
  the migrator gating on `condition: service_healthy` would be released as soon
  as ClickHouse is *running*, not when it answers, so the migrator can still lose
  the race it is supposed to be protected from.
- The collector's own gate — `/signoz-otel-collector migrate sync check` — is a
  healthcheck command. There is no way to express it at all.

Parsing `healthcheck` (`test`, `interval`, `timeout`, `retries`, `start_period`,
`disable`) and passing it to `bollard` is what makes `service_healthy` mean what
it says. Tracked in [TODO.md](../TODO.md).

### Resolved since this was written

Three blockers this document originally recorded have since been fixed:

- **`depends_on` is honored.** Both the list form and the long form with
  `condition: service_started | service_healthy |
  service_completed_successfully` parse into `DependsOn` /
  `DependencyCondition`, and services boot in dependency order.
  `validate_dependency_graph` rejects unknown dependencies and cycles up front
  rather than deadlocking. (`acfab00`)
- **One-shot containers no longer stall the barrier.** The readiness loop in
  `boot_service` treats a terminal state (`EXITED` / `DEAD`) as launched, so a
  job that finishes in under a second is not waited on forever (`acfab00`); the
  loop is also capped and returns `DockerModuleError::ServiceStartTimeout`
  instead of spinning (`d510d60`). `init-clickhouse` and the migrator are
  expressible on this point.
- **`restart` is honored.** The old hardcoded 5s crash-restart loop is gone;
  `resolve_restart_policy` maps the compose key onto a Docker `RestartPolicy`
  applied via `HostConfig`, so retry behaviour is the daemon's job and no longer
  masks a genuine ordering failure with a burst of restarts. (`57b8094`)

### Smaller sharp edges

- **`command` must be a scalar string.** A YAML sequence fails deserialization of
  the *entire file*, aborting startup. The deleted `logs_signoz.yaml` used
  `command: [ "-config.file=/etc/tempo.yaml" ]` and would not parse today.
- **`ports` are always `/tcp`.** `host:container` and `ip:host:container` both
  parse now, so a loopback-only binding like `haproxy.yaml`'s
  `"127.0.0.1:5432:5432"` is honored. But the protocol is hardcoded — a `/udp`
  suffix, a container-only port, and port ranges are all still unsupported.
- **`healthcheck`, `labels`, `ulimits`, `tty` are all ignored.** Upstream sets
  `ulimits: nofile: 262144` on ClickHouse; that must be arranged at the daemon
  level instead.

---

## 5. Two viable shapes

### Full port — 6 services

Faithful to upstream: replication on, `cluster` remote_servers entry, ZooKeeper,
and the `histogramQuantile` UDF. Requires vendoring four ClickHouse config files
from `deploy/common/clickhouse/` at v0.129.0:

| File | Purpose | Mount target |
|---|---|---|
| `cluster.xml` | ZooKeeper node + `cluster` remote_servers | `/etc/clickhouse-server/config.d/cluster.xml` |
| `custom-function.xml` | Declares the `histogramQuantile` executable UDF | `/etc/clickhouse-server/custom-function.xml` |
| `config.xml` | Full server config (~56 KB) | `/etc/clickhouse-server/config.xml` |
| `users.xml` | Passwordless `default` user, `default` profile/quota | `/etc/clickhouse-server/users.xml` |

`custom-function.xml` is picked up because `config.xml` sets
`<user_defined_executable_functions_config>*function.xml</...>` and
`<user_scripts_path>/var/lib/clickhouse/user_scripts/</...>` — which is also why
`init-clickhouse` and `clickhouse` must share that directory. Note that upstream
additionally sets `CLICKHOUSE_SKIP_USER_SETUP=1` on the ClickHouse service.

### Slim — 4 services

Drop `zookeeper-1` and `init-clickhouse`; set
`SIGNOZ_OTEL_COLLECTOR_CLICKHOUSE_REPLICATION=false`. Lighter, but off the
supported path — the schema migrator's behaviour without replication is not
something upstream tests — and histogram percentile queries stop working without
the UDF.

### Names must be rewritten consistently

Prefixing service keys is not free. `cluster.xml` hardcodes the ZooKeeper host as
`zookeeper-1` and the ClickHouse replica as `clickhouse`, and the DSNs are
hardcoded in three places:

- `SIGNOZ_TELEMETRYSTORE_CLICKHOUSE_DSN=tcp://clickhouse:9000` (signoz)
- `SIGNOZ_OTEL_COLLECTOR_CLICKHOUSE_DSN=tcp://clickhouse:9000` (collector, migrator)
- Every exporter DSN in `otel-collector-config.yaml`
  (`tcp://clickhouse:9000/signoz_traces`, `…/signoz_metrics`, `…/signoz_logs`,
  `…/signoz_meter`, `…/signoz_metadata`)

plus `otel-collector-opamp-config.yaml`, whose entire content is
`server_endpoint: ws://signoz:4320/v1/opamp`. All of these must be updated in
lockstep with the chosen keys.

---

## 6. Prerequisites before writing any YAML

Recommended order:

1. **Add `entrypoint: Option<String>`** to `ServiceConfig` and wire it to
   `ContainerCreateBody.entrypoint`. Without it the migrator is not expressible.
2. **Parse `healthcheck`** and pass it to `bollard`, so the
   `condition: service_healthy` gates SigNoz's ordering contract depends on
   actually wait for readiness rather than for "running".
3. Then vendor the ClickHouse configs under `containers/config/signoz/`, write
   `containers/signoz.yaml` with `signoz-`-prefixed keys, add the `signoz_out`
   sink to `config/vector.yaml`, add the HAProxy ACL entry, and record the new
   hostname in the certbot SAN list.
4. Remove the `# signoz:` placeholder from `observability_ui.yaml`.

Steps 1 and 2 are independently useful — they fix latent problems that exist
regardless of whether SigNoz is ever adopted. The two other prerequisites this
document originally listed (one-shot handling and `depends_on`) have since
landed; see §4.

### Unrelated, but found while surveying

[sonarqube.yaml](../crates/core/containers/sonarqube.yaml) is commented out in its
entirety, leaving a YAML document that parses to `null`. Deserializing `null` into
`ComposeFile` is an error, and `find_docker_definitions` propagates it with `?` —
which would abort startup for the whole application. `observability_generator.yaml`
avoids this only because it keeps a live `services:` key. Worth confirming against
a real run before adding another mostly-commented file.
