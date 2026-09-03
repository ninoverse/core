# Redpanda: an authenticated Kafka broker on three networks

**Status: verified against a running broker.** Two gaps remain, both outside the
repository — the deployed certificate does not yet cover
`kafka.nicolapasqualini.it`, and nothing places `chain.pem`/`key.pem` into
`/home/nino/ninoverse/certs`. See [§3](#3-prerequisites).

This document is the runbook for initialising and starting the broker, and the
record of *why* its setup looks the way it does. It replaced a single-listener
`apache/kafka` container that spoke cleartext to anyone on the LAN.

Almost nothing here is a preference. The three listeners are forced by how Kafka
clients reconnect, the config lives in a volume because `rpk` rewrites it, the
certificates are copied rather than mounted because two processes need the same
private key under different uids, and the whole service sits outside the
orchestrator because of two loader gaps in
[docker.rs](../crates/core/src/docker.rs).
[§8](#8-loader-gaps-and-what-they-cost-here) says what to delete when those close.

---

## 1. The access model, and why there are three listeners

A Kafka client does not keep talking to the address it dialled. It bootstraps,
receives each broker's **advertised** address in the metadata response, and
reconnects to *that*. An advertised address is therefore only correct from one
vantage point, and this broker serves three.

| Listener | Binds | Advertises | Transport | Auth | Who reaches it |
|---|---|---|---|---|---|
| `internal` | `0.0.0.0:9092` | `kafka:9092` | plaintext | SASL/SCRAM | containers on `ninoverse-network` |
| `host` | `0.0.0.0:19092` | `localhost:19092` | plaintext | SASL/SCRAM | the `core` binary, over loopback |
| `external` | `0.0.0.0:29092` | `kafka.nicolapasqualini.it:9092` | TLS | SASL/SCRAM + OAUTHBEARER | the internet — IoT, mobile |

That single fact determines the rest:

- **Plaintext on two of them is deliberate.** Both are host-local — a docker
  bridge and loopback — and SCRAM is challenge-response, so no password crosses
  either. Terminating TLS there would buy nothing and couple both to the 90-day
  certificate rotation.
- **The broker terminates its own TLS.** HAProxy is a pure TCP relay for 9092,
  exactly as it already is for kanidm. Nothing decrypts Kafka traffic in transit.
- **The advertised port is 9092, not 29092.** Clients reconnect to the port
  HAProxy publishes, not the container port behind it. Changing the published
  port without changing `advertised_kafka_api` breaks every client *after* a
  successful bootstrap, which looks like an intermittent failure rather than a
  misconfiguration.

Two internet paths exist, because one is not enough:

```
native   client → HAProxy :9092 (TCP passthrough) → kafka:29092  (TLS + SASL, terminated by redpanda)
REST     client → HAProxy :443 → APISIX (bearer token) → kafka:8082  (pandaproxy)
```

The REST path is for devices that cannot speak the Kafka protocol, and for
mobile networks that block 9092 outright. It costs no new ingress — APISIX is
already behind HAProxy on 443 — and it is why there is no need to multiplex
Kafka over 443 by SNI.

> **Authorisation is ACLs, not group membership.** `kafka_enable_authorization`
> is on, so authenticating gets you a principal; an ACL is what makes that
> principal able to read anything. A user with no ACLs authenticates fine and
> then sees `TOPIC_AUTHORIZATION_FAILED` on everything.

---

## 2. Moving parts

| File | Role |
|---|---|
| [containers/standalone/redpanda.yaml](../crates/core/containers/standalone/redpanda.yaml) | The broker and its init container. Runs **outside** the orchestrator — see [§8](#8-loader-gaps-and-what-they-cost-here). |
| [containers/config/redpanda.yaml](../crates/core/containers/config/redpanda.yaml) | Node config: listeners, advertised addresses, TLS, the in-process proxy clients. Read every boot. |
| [containers/config/redpanda-bootstrap.yaml](../crates/core/containers/config/redpanda-bootstrap.yaml) | Cluster properties, seeded **once** on an empty data volume. |
| [containers/config/haproxy.cfg](../crates/core/containers/config/haproxy.cfg) | `frontend kafka_in` (passthrough to 29092, readiness on 9644) and `acl is_apisix` for the REST host. |
| [containers/haproxy.yaml](../crates/core/containers/haproxy.yaml) | Publishes 9092 on `0.0.0.0` deliberately; postgres is loopback-only. |
| [init_kanidm_apisix/config/default.yml](../crates/init_kanidm_apisix/config/default.yml) | The `kafka` OAuth2 client (`mode: native`) and the `kafka-rest` APISIX route (`mode: api`). |
| [bin/start_redpanda.sh](../bin/start_redpanda.sh) | Boots the broker. Creates `ninoverse-network` if absent. |
| [bin/init_redpanda.sh](../bin/init_redpanda.sh) | Creates SCRAM users and ACLs. Idempotent. |
| [bin/init_certificate.sh](../bin/init_certificate.sh) | Carries `kafka.nicolapasqualini.it` in the SAN list. |
| [crates/core/config/default.yml](../crates/core/config/default.yml) | The app's bootstrap address (`localhost:19092`) and its SASL credentials. |

> **The service key stays `kafka`, and that is load-bearing.** HAProxy's backend
> targets `kafka:29092`, and the container name is what the orchestrator sweeps
> on shutdown. Renaming the service key breaks HAProxy silently — and a probe
> container that happens to be *called* `kafka` will be destroyed by the next
> `core` shutdown, which is easy to mistake for a crash.

The broker's ports, none of which are published except 19092:

| Port | Purpose |
|---|---|
| 9092 / 19092 / 29092 | Kafka API — internal, host, external |
| 9644 | Admin API and Prometheus metrics. HAProxy health-checks `/v1/status/ready` here. Not a data path. |
| 8081 | Schema registry. Schemas live in an internal `_schemas` topic; no extra container. |
| 8082 | HTTP proxy (pandaproxy) — the REST path, reached only through APISIX. |
| 33145 | Internal raft/RPC. Bound but idle on a single node; never expose it. |

The old broker's `9093` KRaft controller listener has no equivalent: Redpanda
builds raft into the core, so that traffic lives on 33145 and the entire
controller-quorum configuration disappears.

---

## 3. Prerequisites

Ordered. Each is a link, not an instruction.

| # | Prerequisite | How |
|---|---|---|
| 1 | Certificate covers `kafka.nicolapasqualini.it` | [bin/init_certificate.sh](../bin/init_certificate.sh), then rebuild `/home/nino/ninoverse/certs/` |
| 2 | `chain.pem` + `key.pem` present, owned `8443:8443`, key mode `600` | Manual — see the gap below |
| 3 | `RP_ADMIN_PASSWORD` chosen | Any strong value. It creates the superuser **and** is substituted into the node config for the in-process proxy clients; the two must match. |
| 4 | *(OAUTHBEARER only)* `kafka` client provisioned in kanidm | `cargo run -p init_kanidm_apisix` **from the repo root** — `secrets_dir` is CWD-relative |

Nothing else. In particular the orchestrator does **not** need to be running:
`bin/start_redpanda.sh` creates `ninoverse-network` itself.

**Known gap at steps 1–2.** The deployed certificate carries eleven explicit
SANs and no wildcard, and `kafka.` is not among them — the SAN was added to
`init_certificate.sh` but the certificate has not been reissued. Separately,
nothing in the repository copies certbot's output into
`/home/nino/ninoverse/certs` or sets the `8443:8443` ownership that both kanidm
and this broker depend on; that step exists only on the deployed host. Until
both land, the `internal` and `host` listeners work fully and only the
`external` listener is unusable.

**Step 4 is not a boot dependency.** The broker starts fine without it and
retries OIDC discovery every five seconds, logging an `ERROR` each time. That is
expected and harmless — SASL/SCRAM is unaffected; only OAUTHBEARER waits.

---

## 4. First-time initialisation

Two commands, in this order. Neither is interactive.

```bash
RP_ADMIN_PASSWORD=... ./bin/start_redpanda.sh
RP_ADMIN_PASSWORD=... CORE_APP_PASSWORD=... ./bin/init_redpanda.sh
```

### 4.1 What `start_redpanda.sh` does

Creates `ninoverse-network` if it is missing, then brings up `init-redpanda-config`
followed by the broker.

The init container exists for two reasons that are not obvious:

| Why | Detail |
|---|---|
| **`rpk` rewrites its own config on every start** | It writes a temporary file into `/etc/redpanda` and renames over `redpanda.yaml`. A rename can never replace a bind-mounted file, so the config is *copied* into a volume instead of mounted. `:ro` is not the problem; bind-mounting at all is. |
| **The certificate cannot be shared by uid** | `key.pem` is mode `600` owned by `8443` for kanidm, and redpanda runs as uid `101`. Rather than loosen permissions certbot resets on every renewal, the init container — running as root — copies the pair into the config volume and chowns it. |

It also substitutes `__RP_ADMIN_PASSWORD__` in the copied config from
`$RP_ADMIN_PASSWORD`, so no credential is committed. The substitution happens
inside the container, through the environment, so the password never appears in
a command line where `docker inspect` would expose it.

### 4.2 What `init_redpanda.sh` does

Waits for `Healthy: true`, then creates the `core-app` SCRAM user and its ACLs:
everything on topic `ninoverse`, everything on group `ninoverse`, and
cluster-level `create,describe` so the app's admin client can create its own
topic.

For a device, give it its **own** user rather than sharing that one — a shared
credential cannot be revoked without breaking the whole fleet — and prefer
produce-only where it never consumes:

```bash
docker exec -e RPK_USER=admin -e RPK_PASS="$RP_ADMIN_PASSWORD" \
  -e RPK_SASL_MECHANISM=SCRAM-SHA-512 kafka \
  rpk security user create <device> --password <pw> --mechanism SCRAM-SHA-512

docker exec -e RPK_USER=admin -e RPK_PASS="$RP_ADMIN_PASSWORD" \
  -e RPK_SASL_MECHANISM=SCRAM-SHA-512 kafka \
  rpk security acl create --allow-principal User:<device> \
    --operation write,describe --topic <its-topic>
```

### 4.3 Point the app at it

`crates/core/config/default.yml` ships `sasl_password: "changeme"`. Override it
rather than editing it in:

```bash
export APP__KAFKA__SASL_PASSWORD=<CORE_APP_PASSWORD>
```

---

## 5. Routine start and stop

The broker comes up **before** the orchestrator, and this is not a style choice:
`init_kafka` is spawned in `run_threads` and its error propagates out of startup,
so `core` exits — removing every container it started — when no broker answers.

```bash
RP_ADMIN_PASSWORD=... bin/start_redpanda.sh   # broker first
cd crates/core && cargo run                   # then the orchestrator
```

Nothing in [§4](#4-first-time-initialisation) repeats. Users and ACLs live in the
controller log on `redpanda_redpanda-data`, and the config volume is rebuilt from
the repository on every start — so editing
[config/redpanda.yaml](../crates/core/containers/config/redpanda.yaml) and
restarting is enough to change node configuration.

To stop the broker without disturbing the rest of the stack:

```bash
docker compose -f crates/core/containers/standalone/redpanda.yaml down
```

Add `-v` only when you mean it: that drops the data volume, and with it every
topic, user and ACL — after which
[redpanda-bootstrap.yaml](../crates/core/containers/config/redpanda-bootstrap.yaml)
is read again and [§4](#4-first-time-initialisation) must be redone.

> **Cluster properties are seeded once.** `.bootstrap.yaml` applies only to an
> empty data volume. Editing it against an existing cluster changes nothing —
> use `rpk cluster config set <name> <value>`, which takes effect live.

---

## 6. Verification

```bash
# credentials for everything below
A="-X user=admin -X pass=$RP_ADMIN_PASSWORD -X sasl.mechanism=SCRAM-SHA-512"

# cluster is healthy
docker exec kafka rpk cluster health $A
#   Healthy:  true

# the security properties actually took
docker exec kafka rpk cluster config get sasl_mechanisms
#   - SCRAM
#   - OAUTHBEARER

# anonymous access is refused — the test that matters
docker exec kafka rpk topic list
#   broker closed the connection immediately after a request was issued,
#   which often happens when SASL is required but not provided

# and authenticated access is not
docker exec kafka rpk topic list $A

# the placeholder was substituted, not committed
docker exec kafka grep -c __RP_ADMIN_PASSWORD__ /etc/redpanda/redpanda.yaml
#   0

# the external listener serves the real certificate
docker exec kafka openssl s_client -connect localhost:29092 \
  -servername kafka.nicolapasqualini.it </dev/null 2>/dev/null \
  | openssl x509 -noout -subject -ext subjectAltName

# readiness endpoint haproxy polls, plus the two HTTP surfaces
docker exec kafka curl -s -o /dev/null -w '%{http_code}\n' localhost:9644/v1/status/ready
docker exec kafka curl -s -o /dev/null -w '%{http_code}\n' localhost:8081/subjects
docker exec kafka curl -s -o /dev/null -w '%{http_code}\n' localhost:8082/topics
#   200, 200, 200

# published ports: loopback only
docker port kafka
#   19092/tcp -> 127.0.0.1:19092

# end to end, both paths
docker exec kafka sh -c "echo hello | rpk topic produce ninoverse $A"
docker exec kafka rpk topic consume ninoverse -n 1 -o start $A
docker exec kafka curl -s -X POST localhost:8082/topics/ninoverse \
  -H 'Content-Type: application/vnd.kafka.json.v2+json' \
  -d '{"records":[{"value":"hello-from-rest"}]}'

# ACLs really constrain: this must FAIL for a non-superuser
docker exec kafka rpk topic consume some-other-topic -n 1 \
  -X user=core-app -X pass=$CORE_APP_PASSWORD -X sasl.mechanism=SCRAM-SHA-512
#   TOPIC_AUTHORIZATION_FAILED

# haproxy parses before reloading
docker exec haproxy_router haproxy -c -f /usr/local/etc/haproxy/haproxy.cfg
```

Then from **off this host** — testing the external listener from the host itself
can pass through NAT hairpinning and prove nothing:

```bash
rpk topic list -X brokers=kafka.nicolapasqualini.it:9092 -X tls.enabled=true \
  -X user=<device> -X pass=<pw> -X sasl.mechanism=SCRAM-SHA-512
```

---

## 7. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `UNSUPPORTED_SASL_MECHANISM`, for every mechanism | `sasl_mechanisms` takes mechanism *families*. The value is `SCRAM`, not `SCRAM-SHA-512`. Redpanda accepts an unknown string silently, enables nothing, and rejects every client. | `rpk cluster config set sasl_mechanisms '["SCRAM","OAUTHBEARER"]'`. The per-user mechanism in `RP_BOOTSTRAP_USER` *is* spelled `SCRAM-SHA-512` — different field. |
| `error writing to temporary file: /etc/redpanda/redpanda-…: permission denied` | `rpk` is trying to rewrite its config. Either it was bind-mounted, or the directory is not writable by uid 101. | Copy the config into a volume — [§4.1](#41-what-start_redpandash-does). Never bind-mount `redpanda.yaml`. |
| `Validation errors in node config`, `listener '' improperly configured with ADDR_ANY` | `advertised_rpc_api` defaulted to `0.0.0.0`. An advertised address may not be `ADDR_ANY`. | Set it explicitly to `kafka:33145`. |
| REST returns `503 broker_not_available` while the broker is healthy | `pandaproxy_client` and `schema_registry_client` are Kafka clients *inside* the broker. Once `enable_sasl` is on they must authenticate too, and their seed broker defaults to `0.0.0.0:9092`. | Give both a `brokers` list and SCRAM credentials in the node config. |
| `configuration conflict. Flag '--reserve-memory' is also present in 'rpk.additional_start_flags'` | `rpk` derives `--reserve-memory` and `--overprovisioned` from `developer_mode` and refuses if they are also listed. | Remove them from `additional_start_flags`; keep only `--smp=1`. |
| `--mode dev-container` fails on a read-only config | The flag is implemented by *rewriting* the config file. | Do not pass it. Its runtime half is `rpk.additional_start_flags`, its cluster half is in `.bootstrap.yaml`. |
| `network ninoverse-network declared as external, but could not be found` | On a fresh clone nothing has created it, and the orchestrator cannot get that far without a broker. | Already handled — `start_redpanda.sh` creates it. `create_docker_networks` swallows the 409 later. |
| Broker healthy, but `ERROR … oidc_service.cc … Failed to retrieve metadata` every 5s | The kanidm `kafka` client does not exist yet, or kanidm is down. | Harmless. `cargo run -p init_kanidm_apisix` from the repo root. SCRAM is unaffected. |
| Client authenticates, then `TOPIC_AUTHORIZATION_FAILED` on everything | Authenticated but not authorised. | `rpk security acl list` as admin; grant what the principal needs. |
| A container named `kafka` vanishes mid-test | `core` sweeps containers by service name on shutdown, and `kafka` is one of them. | Name probe containers something else. |
| `core` exits immediately, taking the stack with it | `init_kafka` could not reach a broker. | Start the broker first — [§5](#5-routine-start-and-stop). |
| `Could not find directory of OpenSSL installation` building `core` | The `ssl` feature needs `pkg-config`, which is absent. | Already handled — `rdkafka` uses `ssl-vendored`, which builds OpenSSL from source. Switch to `ssl` after `apt install pkg-config` if the build time matters. |
| Client works at bootstrap, fails on the next request | The advertised address is wrong for that vantage point. | Check `advertised_kafka_api` against the listener the client actually dialled — [§1](#1-the-access-model-and-why-there-are-three-listeners). |

Durability note: `--mode dev-container` would also set `--unsafe-bypass-fsync`
and `write_caching_default`. Both are deliberately **not** enabled — this broker
carries data from devices that cannot replay it, so an acknowledged write has to
survive an unclean shutdown.

---

## 8. Loader gaps, and what they cost here

The broker looks like it should be an ordinary orchestrator-managed service and
is not. That is two unimplemented compose features, tracked in
[TODO.md](../TODO.md) — the same pair that pushed k3s out, plus one more.

| Gap | TODO.md | What it forces here | What to delete when it lands |
|---|---|---|---|
| **`${VAR}` interpolation** | [line 88](../TODO.md#L88) | `RP_ADMIN_PASSWORD` cannot reach a compose file, so the broker cannot be orchestrator-managed at all | Half of the reason for `containers/standalone/` |
| **`command` list form** | [line 104](../TODO.md#L104) | `command: Option<String>` is shlex-split, so the init container's multi-line shell script cannot be expressed | The other half |
| **Network `external:`** | [line 160](../TODO.md#L160) | `NetworkConfig` reads only `driver`, so moving this back would make the orchestrator re-create `ninoverse-network` instead of joining it | Blocks the two rows above — see the note below |
| **`healthcheck` + boot gate** | [line 72](../TODO.md#L72) | Nothing can gate `core` on the broker being *ready*; it only knows the container is running, so startup order is enforced by hand | The ordering warning in [§5](#5-routine-start-and-stop) |
| **Volume `external:`** | [line 163](../TODO.md#L163) | The data volume is compose-project-scoped (`redpanda_redpanda-data`), so the orchestrator cannot declare or reuse it | Volume renaming on any future move |
| **`entrypoint`** | [line 102](../TODO.md#L102) | The image entrypoint execs `rpk`, so the redpanda binary cannot be invoked directly to sidestep the config rewrite | The copy-into-volume step in [§4.1](#41-what-start_redpandash-does), partially |

**Moving this back needs three gaps closed, not two.** Interpolation and list-form
`command` are the obvious blockers, but `redpanda.yaml` also declares
`ninoverse-network` as `external: true`. The orchestrator reads only `driver`
from a network definition, so it would create a second one — duplicate resource
names across files being itself an open edge case
([line 191](../TODO.md#L191)). Close the first two alone and the move will appear
to work while quietly duplicating networks.

Worth noting what is *not* a gap: `container_name` is honoured here, because
`standalone/` runs under real compose rather than `docker.rs`. If this ever moves
back, the container takes its **service key** as its name
(`docker.rs:980` discards `container_name`) — which is `kafka` either way, so
HAProxy's backend survives the move unchanged.
