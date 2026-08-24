# Adding SigNoz to the stack

**Status: proposal. Nothing described here is deployed.** The only trace of SigNoz
in the tree today is the bare `# signoz:` placeholder at
[observability_ui.yaml:14](../crates/core/containers/observability_ui.yaml#L14).

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

None of that is expressible today — see §4.

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

> **The `./` prefix is mandatory.** `is_host_path`
> ([docker.rs:121](../crates/core/src/docker.rs#L121)) classifies a bind source
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
  then discarded ([docker.rs:668](../crates/core/src/docker.rs#L668)); the
  container is always named after the YAML key. Service keys are therefore a flat
  global namespace across *every* file in `containers/`. SigNoz's upstream keys
  (`clickhouse`, `zookeeper-1`, `otel-collector`) are far too generic for a shared
  network — prefix them: `signoz-clickhouse`, `signoz-zookeeper`,
  `signoz-otel-collector`.
- **A new file is picked up automatically.** `find_docker_definitions`
  ([docker.rs:295](../crates/core/src/docker.rs#L295)) globs every `*.yaml` in
  `containers/` and flattens the three maps; file names carry no meaning and no
  Rust change is needed to register one. `containers/config/` is skipped because
  it is a directory, not a file.

Because SigNoz brings its own datastore and collector, it does **not** belong in
`observability_ui.yaml` next to Grafana. A dedicated `signoz.yaml` is the right
shape, and the `# signoz:` placeholder should be removed when it lands.

---

## 4. What blocks a faithful port

`ServiceConfig` ([docker.rs:42-54](../crates/core/src/docker.rs#L42-L54)) supports
exactly nine keys: `image`, `ports`, `networks`, `volumes`, `environment`,
`container_name`, `command`, `user` (plus the skipped `mounts`). No struct uses
`deny_unknown_fields`, so everything else is **silently dropped** — the failure is
invisible, not loud.

### No `entrypoint`

Both one-shot jobs rely on `entrypoint: /bin/sh` + `command: -c '<a && b>'`.
Here `command` is a single string run through `shlex::split`
([docker.rs:670](../crates/core/src/docker.rs#L670)), so `&&` arrives as a literal
argument to the image's own entrypoint rather than being interpreted by a shell.

- `init-clickhouse` can be worked around: swap the image for one with an empty
  ENTRYPOINT and use `command: 'sh -c "..."'` — the trick `kanidm.yaml`'s
  `init-volume` already uses with `busybox:1.38.0`.
- The **migrator cannot be**. Its image entrypoint is the collector binary, so
  `migrate bootstrap && migrate sync up && migrate async up` would have to be
  split across three separate containers with no way to order them.

Adding `entrypoint: Option<String>` to `ServiceConfig` and passing it through to
`ContainerCreateBody.entrypoint` is a small, contained change that removes this
entire class of workaround.

### No `depends_on`

`depends_on` deserializes into nothing. Every service is spawned concurrently into
a `JoinSet` ([docker.rs:507](../crates/core/src/docker.rs#L507)) with no ordering
at all — `kanidm.yaml`'s `depends_on: { init-volume: { condition:
service_completed_successfully } }` is already being ignored today.

In practice the crash watcher ([docker.rs:554](../crates/core/src/docker.rs#L554))
re-`start`s any non-running container every 5 seconds, so a migrator that starts
before ClickHouse fails, exits, and is retried until ClickHouse answers. It
converges — but by retry, not by design, and the logs will show a burst of
failures on every boot.

### One-shot containers stall the startup barrier

This is the sharpest edge. After `start_container` succeeds, `boot_service` polls
until `state.running == true`
([docker.rs:769-780](../crates/core/src/docker.rs#L769-L780)):

```rust
loop {
    let inspect = docker.inspect_container(service_name, None).await.unwrap();
    if let Some(state) = inspect.state {
        if state.running.unwrap_or(false) { break; }
    }
    sleep(Duration::from_secs(1)).await;
}
```

A job that finishes in under a second is never observed running, so the loop spins
forever, the service never signals the `Barrier`, and *"All Docker definitions are
up"* is never reached. `init-clickhouse` and the migrator would both hit this.
`kanidm.yaml`'s `init-volume` (a `chown` that returns immediately) is exposed to it
today.

`boot_service` needs to treat a container that has exited — ideally with status 0 —
as booted, not keep waiting.

### Smaller sharp edges

- **`command` must be a scalar string.** A YAML sequence fails deserialization of
  the *entire file*, aborting startup. The deleted `logs_signoz.yaml` used
  `command: [ "-config.file=/etc/tempo.yaml" ]` and would not parse today.
- **`ports` with a host-IP prefix are silently dropped.** Only `parts.len() == 2`
  is handled ([docker.rs:643](../crates/core/src/docker.rs#L643)); every binding
  is forced to `0.0.0.0` and `/tcp`. `haproxy.yaml`'s `"127.0.0.1:5432:5432"` is
  already being discarded. If any SigNoz port should be loopback-only, this
  parser cannot express it.
- **`restart`, `healthcheck`, `labels`, `ulimits`, `tty` are all ignored.**
  Upstream sets `ulimits: nofile: 262144` on ClickHouse; that must be arranged at
  the daemon level instead.

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
2. **Fix one-shot handling in `boot_service`** — treat an exited container as
   booted instead of polling `running == true` forever. This also unbreaks
   `kanidm.yaml`'s `init-volume`.
3. Then vendor the ClickHouse configs under `containers/config/signoz/`, write
   `containers/signoz.yaml` with `signoz-`-prefixed keys, add the `signoz_out`
   sink to `config/vector.yaml`, add the HAProxy ACL entry, and record the new
   hostname in the certbot SAN list.
4. Remove the `# signoz:` placeholder from `observability_ui.yaml`.

Steps 1 and 2 are independently useful — they fix latent problems that exist
regardless of whether SigNoz is ever adopted.

### Unrelated, but found while surveying

[sonarqube.yaml](../crates/core/containers/sonarqube.yaml) is commented out in its
entirety, leaving a YAML document that parses to `null`. Deserializing `null` into
`ComposeFile` is an error, and `find_docker_definitions` propagates it with `?` —
which would abort startup for the whole application. `observability_generator.yaml`
avoids this only because it keeps a live `services:` key. Worth confirming against
a real run before adding another mostly-commented file.
