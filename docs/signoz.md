# Adding SigNoz to the stack

**Status: proposal. Nothing described here is deployed.** The only trace of SigNoz
in the tree today is the bare `# signoz:` placeholder at
[observability_ui.yaml:14](../crates/core/containers/observability_ui.yaml#L14).

> **Note:** every `crates/core/containers/…` link below resolves only on the
> `wip/add-docker-definitions-and-configurations` branch. On `main` that
> directory holds just a `.gitkeep`; the compose definitions are not merged yet.

This document records what SigNoz actually needs and what the repository
already provides that it can reuse.

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

Both halves are expressible as-is: `core` hands the compose files to
`docker compose`, which runs healthchecks and honors every `depends_on`
condition — see §4.

---

## 3. What can be reused as-is

This is the part that keeps a SigNoz port small.

### Network

`ninoverse-network` is declared once, in
[core.yaml](../crates/core/containers/core.yaml). SigNoz's own `signoz-net`
should simply be dropped; every SigNoz service joins `ninoverse-network` like
everything else. Docker's embedded DNS then resolves service names across the
whole stack, which is what HAProxy and Vector already rely on.

Every file in `containers/` is passed to Compose as one project, so a network
declared once is visible to all of them. A new file should just not re-declare
the network, and must not mark it `external: true`: Compose then expects a
network literally named `ninoverse-network` to exist already, while the project
creates it as `ninoverse_ninoverse-network`.

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

> **The `./` prefix is mandatory.** In the short volume syntax Compose treats a
> source as a host path only when it starts with `.`, `/` or `~`. A source written
> as `config/signoz/cluster.xml` is read as a *named volume*, not a bind. This is
> precisely the bug the deleted `logs_signoz.yaml` had. Relative sources resolve
> against `containers/`, the project directory `core` passes to Compose.

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
- **Service keys are one namespace across every file.** The service key is the
  DNS name on the shared network, and a key repeated in two files is *merged*
  by Compose into one service, not reported. Every file sets `container_name:`
  as well. SigNoz's upstream keys
  (`clickhouse`, `zookeeper-1`, `otel-collector`) are far too generic for a shared
  network — prefix them: `signoz-clickhouse`, `signoz-zookeeper`,
  `signoz-otel-collector`.
- **A new file is picked up automatically.** `find_compose_files` (in
  [compose.rs](../crates/core/src/compose.rs)) collects every `*.yaml` and
  `*.yml` directly inside `containers/`; file names carry no meaning and no Rust
  change is needed to register one. Subdirectories such as `containers/config/`
  are not descended into.

Because SigNoz brings its own datastore and collector, it does **not** belong in
`observability_ui.yaml` next to Grafana. A dedicated `signoz.yaml` is the right
shape, and the `# signoz:` placeholder should be removed when it lands.

---

## 4. What Compose now handles

This section used to list the gaps in `core`'s own compose parser that blocked
a faithful port: no `healthcheck` (so `condition: service_healthy` degraded to
"running" and the collector's `migrate sync check` gate was inexpressible), no
`ulimits`, and — before they landed — no `entrypoint`, no list-form `command`,
no `depends_on`, no one-shot handling. That parser is gone. `core` now passes
the files to `docker compose up --wait` (`start_compose_stack` in
[compose.rs](../crates/core/src/compose.rs)), so all of upstream's keys mean
what they mean upstream:

- `healthcheck` runs, `condition: service_healthy` waits for it, and the
  collector's `migrate sync check` can be its healthcheck.
- `init-clickhouse` and the migrator are ordinary one-shot services gated by
  `condition: service_completed_successfully`; `up --wait` treats a one-shot
  that exits 0 as done and fails startup on a non-zero exit (both verified).
- `entrypoint: /bin/sh` + `command: -c '<a && b>'`, list-form `command`,
  `ulimits: nofile: 262144` and `/udp` ports port across unchanged.

One caveat carries over: `up --wait` cannot tell that a service *without* a
healthcheck is crash-looping under a restart policy. Keep upstream's
healthchecks on `clickhouse`, `zookeeper-1` and `signoz` rather than trimming
them.

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

No Rust change is needed any more; the loader prerequisites this document
originally listed are covered by Compose (§4). Recommended order:

1. Vendor the ClickHouse configs under `containers/config/signoz/`, write
   `containers/signoz.yaml` with `signoz-`-prefixed keys, add the `signoz_out`
   sink to `config/vector.yaml`, add the HAProxy ACL entry, and record the new
   hostname in the certbot SAN list.
2. Remove the `# signoz:` placeholder from `observability_ui.yaml`.

### Unrelated, but found while surveying

[sonarqube.yaml](../crates/core/containers/sonarqube.yaml) is commented out in its
entirety, and `observability_generator.yaml` keeps a bare `services:` with every
entry commented out. Compose rejects both (`empty compose file`, `services must
be a mapping`), and because every file is one project, either one aborts startup
for the whole stack (verified with `docker compose config`). Both need to go
before the wip branch merges — tracked in [TODO.md](../TODO.md). Do not add
another mostly-commented file.
