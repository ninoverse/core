# Headlamp: a Kubernetes UI behind kanidm

**Status: deployed and working.** One gap remains — the deployed certificate does
not yet cover `headlamp.nicolapasqualini.it`, so the browser login flow trips a
name mismatch. See [§3](#3-prerequisites).

This document is the runbook for initialising and starting Headlamp, and the
record of *why* its setup looks the way it does. Three of its steps exist nowhere
in code — a `chown`, a hand-built kubeconfig, and a `ClusterRoleBinding` that
lives only in cluster state — and two of its oddities are not design choices but
consequences of loader gaps in [docker.rs](../crates/core/src/docker.rs).
[§8](#8-loader-gaps-and-what-they-cost-here) says what to delete when those close.

---

## 1. The auth model, and why it reaches into k3s

Headlamp keeps no user table. After the OIDC authorization-code flow it takes the
`id_token` and puts it on the wire as `Authorization: Bearer <id_token>` towards
the Kubernetes API server.

That single fact determines everything else:

- **k3s itself must trust kanidm.** The `--kube-apiserver-arg=oidc-*` flags on
  [k3s.yaml](../crates/core/containers/standalone/k3s.yaml) are not optional
  decoration; without them the token authenticates nothing.
- **APISIX stays out of the request path.** Headlamp speaks OIDC natively, so its
  entry in [default.yml](../crates/init_kanidm_apisix/config/default.yml) is
  `mode: native`. `build_route` refuses to emit a route for that mode
  ([route.rs:107-112](../crates/apisix/src/route.rs#L107-L112)), so ingress is
  HAProxy's job — exactly as for grafana. Gating it at the gateway instead would
  log the user in and still leave Headlamp with no cluster credential.
- **Authorisation is RBAC, not group membership in kanidm.** Being in
  `headlamp_admins` gets you a token; a `ClusterRoleBinding` is what makes that
  token able to read anything.

The identity chain, end to end:

```
browser → HAProxy (TLS, X-Forwarded-Proto) → headlamp:4466
       → kanidm /oauth2/openid/headlamp        (authorization code + PKCE)
       → id_token, preferred_username = "nino"
       → k3s apiserver, oidc-username-prefix   → user "oidc:nino"
       → ClusterRoleBinding headlamp-oidc-admin → cluster-admin
```

Two details that are easy to get wrong and hard to debug:

| Detail | Why |
|---|---|
| OIDC client id / secret / issuer live in the **kubeconfig**, not env vars | For a file-sourced context Headlamp reads them from the context's `auth-provider` block. The `HEADLAMP_CONFIG_OIDC_*` equivalents are ignored here, and rejected outright unless `-in-cluster` or `-oidc-use-cookie` is set. `HEADLAMP_CONFIG_OIDC_USE_PKCE` is the exception — it is global, and required, because kanidm mandates PKCE. |
| `oidc-signing-algs=ES256` on the apiserver | kanidm signs ES256. Headlamp copes by itself (go-oidc adopts whatever the discovery document advertises); the apiserver hardcodes an RS256 default and would reject every token. |

---

## 2. Moving parts

| File | Role |
|---|---|
| [containers/headlamp.yaml](../crates/core/containers/headlamp.yaml) | The container. Orchestrator-managed, on `ninoverse-network`. |
| [containers/standalone/k3s.yaml](../crates/core/containers/standalone/k3s.yaml) | The cluster, plus the apiserver OIDC flags and `--tls-san=k3s-server`. Runs **outside** the orchestrator — see [§8](#8-loader-gaps-and-what-they-cost-here). |
| [containers/config/haproxy.cfg](../crates/core/containers/config/haproxy.cfg) | `acl is_headlamp`, `use_backend headlamp_backend`, and the backend itself. |
| [init_kanidm_apisix/config/default.yml](../crates/init_kanidm_apisix/config/default.yml) | The `headlamp` OAuth2 client (`mode: native`) and the `headlamp_admins` group. |
| [bin/start_k3s.sh](../bin/start_k3s.sh) | Boots the cluster. |
| [bin/init_certificate.sh](../bin/init_certificate.sh) | Carries `headlamp.nicolapasqualini.it` in the SAN list. |
| `secrets/headlamp/config` | The kubeconfig. Gitignored, built by hand — [§4](#4-first-time-initialisation). |

> **The service key is the DNS name, not `container_name`.** The orchestrator
> parses `container_name:` and throws it away (`docker.rs:980`), naming every
> container after its service key. `headlamp_backend` therefore targets
> `headlamp:4466`. Renaming the service key breaks HAProxy silently.

`X-Forwarded-Proto https` on that backend is load-bearing, not boilerplate:
Headlamp builds its own OIDC callback URL from it, and without it hands kanidm an
`http://` redirect that kanidm rejects.

---

## 3. Prerequisites

Ordered. Each is a link, not an instruction — these are already encoded elsewhere.

| # | Prerequisite | How |
|---|---|---|
| 1 | Orchestrator up (etcd, networks, kanidm, HAProxy) | `cd crates/core && cargo run` |
| 2 | kanidm CLI has a session | [bin/init_kanidm.sh](../bin/init_kanidm.sh) — first boot only |
| 3 | `headlamp` client provisioned, `secrets/OIDC_SECRET_HEADLAMP` written | `cargo run -p init_kanidm_apisix` **from the repo root** — `secrets_dir` is CWD-relative |
| 4 | Cluster up | [bin/start_k3s.sh](../bin/start_k3s.sh) — must follow 1; `etcd-backend` and `ninoverse-network` are joined as `external` |
| 5 | Certificate covers the subdomain | [bin/init_certificate.sh](../bin/init_certificate.sh), then rebuild `/home/nino/ninoverse/certs/` |

**Known gap at step 5.** The deployed certificate covers neither `headlamp.` nor
`registry.`, and carries an `artifact.` entry that is absent from
`init_certificate.sh` — the script's domain list and the live lineage have
drifted. Until a reissue lands, everything below works but the browser login
trips a certificate-name mismatch. DNS itself is fine: the stale apex `A` record
that previously broke ACME validation has been removed.

---

## 4. First-time initialisation

Steps 1–3 of [§3](#3-prerequisites) must be done, and k3s must be running — the
kubeconfig needs both the kanidm secret *and* the cluster CA, so it cannot exist
any earlier. This is also why [headlamp.yaml](../crates/core/containers/headlamp.yaml)
binds the **directory** `secrets/headlamp/` and not the file: Docker materialises
a missing bind source as a directory, so a file bind would create a *directory*
where the kubeconfig belongs and the heredoc below would fail with
`Is a directory`.

On the very first boot Headlamp logs `kubeconfig not found` and serves an empty
cluster list. That is expected — the error is logged, not fatal.

### 4.1 Take ownership of the mount point

```bash
sudo chown "$USER:$USER" secrets/headlamp
```

Docker created the directory as `root:root` when it materialised the bind, so the
next step fails with permission denied without this. One time only.

### 4.2 Build the kubeconfig

From the repo root:

```bash
CA=$(docker exec k3s-server sed -n 's/.*certificate-authority-data: //p' /etc/rancher/k3s/k3s.yaml)
SECRET=$(cat secrets/OIDC_SECRET_HEADLAMP)

cat > secrets/headlamp/config <<EOF
apiVersion: v1
kind: Config
current-context: ninoverse
clusters:
  - name: ninoverse
    cluster:
      server: https://k3s-server:6443
      certificate-authority-data: ${CA}
users:
  - name: kanidm
    user:
      auth-provider:
        name: oidc
        config:
          client-id: headlamp
          client-secret: ${SECRET}
          idp-issuer-url: https://auth.nicolapasqualini.it/oauth2/openid/headlamp
          scope: profile,email,groups
contexts:
  - name: ninoverse
    context: { cluster: ninoverse, user: kanidm }
EOF

chmod 644 secrets/headlamp/config
docker restart headlamp
```

| Line | Why |
|---|---|
| `server: https://k3s-server:6443` | Not the `127.0.0.1` k3s writes. Headlamp dials over `ninoverse-network`, which is why the server carries `--tls-san=k3s-server`. |
| `auth-provider` block | Where Headlamp reads OIDC for a file-sourced context. Also what makes `AuthType()` return `oidc`, so the UI offers a kanidm sign-in instead of a token box. |
| `scope` omits `openid` | Headlamp prepends it. |
| `chmod 644` | The container runs as a non-root `headlamp` user and cannot otherwise read the file. |
| `docker restart` | The kubeconfig is read at startup only. |

Sanity: the CA is ~756 characters, the secret 48, the result ~1321 bytes. The
heredoc is unquoted so `${CA}` and `${SECRET}` expand — check the secret for
`$`, `` ` ``, `\` or `"` if a future rotation ever produces one.

### 4.3 Grant RBAC

Authentication without this gets you a UI where every resource is `Forbidden`.

```bash
cat > /tmp/headlamp-rbac.yaml <<'EOF'
apiVersion: rbac.authorization.k8s.io/v1
kind: ClusterRoleBinding
metadata:
  name: headlamp-oidc-admin
subjects:
  - kind: User
    name: "oidc:nino"
    apiGroup: rbac.authorization.k8s.io
roleRef:
  kind: ClusterRole
  name: cluster-admin
  apiGroup: rbac.authorization.k8s.io
EOF

docker cp /tmp/headlamp-rbac.yaml k3s-server:/tmp/headlamp-rbac.yaml
docker exec k3s-server kubectl apply -f /tmp/headlamp-rbac.yaml
docker exec k3s-server rm -f /tmp/headlamp-rbac.yaml
```

**Do not use `docker exec -i … kubectl apply -f -`.** It hangs; see
[§7](#7-troubleshooting).

The subject binds the *user*, which is deterministic: `provision_client` applies
`prefer-short-username`, so `preferred_username` is `nino`, and
`oidc-username-prefix=oidc:` makes it `oidc:nino`. A group binding would be
nicer, but kanidm's `groups` claim emits both SPN and UUID, so the subject would
be `oidc:headlamp_admins@nicolapasqualini.it` — confirm against a real token
before switching.

This binding lives only in cluster state. It survives restarts via the
`k3s-server-data` volume, but wiping that volume loses it *and* mints a new CA,
which means redoing [§4.2](#42-build-the-kubeconfig) as well.

---

## 5. Routine start and stop

Once initialised, a normal boot is two commands:

```bash
cd crates/core && cargo run        # orchestrator: etcd, networks, kanidm, HAProxy, headlamp
bin/start_k3s.sh                   # the cluster — must come second
```

Nothing in [§4](#4-first-time-initialisation) repeats. `secrets/headlamp/config`
persists on the host and the `ClusterRoleBinding` persists in the cluster.

To stop the cluster without disturbing the rest of the stack:

```bash
docker compose -f crates/core/containers/standalone/k3s.yaml down
```

Re-run [§4.2](#42-build-the-kubeconfig) after any
`cargo run -p init_kanidm_apisix` that rotates `OIDC_SECRET_HEADLAMP` — the
kubeconfig carries a copy, and nothing syncs it.

---

## 6. Verification

```bash
# cluster is healthy
docker exec k3s-server kubectl get nodes
#   NAME           STATUS   ROLES           VERSION
#   57eb2c7627d0   Ready    <none>          v1.36.4+k3s1
#   aa54df0e3ecf   Ready    control-plane   v1.36.4+k3s1

# the apiserver took the OIDC flags
docker logs k3s-server 2>&1 | grep -i oidc

# this must be a DIRECTORY holding a file called `config`
ls -l secrets/headlamp/

# headlamp loaded the context — must NOT say "kubeconfig not found"
docker logs --tail 20 headlamp

# RBAC actually grants
docker exec k3s-server kubectl auth can-i '*' '*' --all-namespaces --as="oidc:nino"
#   yes

# kanidm client and redirect
docker exec kanidm-cli kanidm system oauth2 get headlamp --name idm_admin

# discovery reachable from inside the network, the path headlamp takes
docker exec apisix-curl curl -sS \
  https://auth.nicolapasqualini.it/oauth2/openid/headlamp/.well-known/openid-configuration | head

# haproxy parses before reloading
docker exec haproxy haproxy -c -f /usr/local/etc/haproxy/haproxy.cfg
```

Then in a browser: `https://headlamp.nicolapasqualini.it` → sign in → kanidm as
`nino` → back at `/oidc-callback` → nodes and pods listed. Blocked on the
certificate gap in [§3](#3-prerequisites).

---

## 7. Troubleshooting

| Symptom | Cause | Fix |
|---|---|---|
| `kubectl apply -f -` never returns | `-f -` blocks until stdin closes; through `docker exec -i` the heredoc often never reaches it. A shell sitting at its `>` continuation prompt looks identical. | Apply from a file via `docker cp` — [§4.3](#43-grant-rbac). |
| `cat > secrets/headlamp/config` → permission denied | Docker created the bind mount point as `root:root`. | [§4.1](#41-take-ownership-of-the-mount-point). |
| `cat >` → `Is a directory` | Something bound `secrets/headlamp/config` as a *file* path before it existed, so Docker created it as a directory. | `rmdir` it and restore the directory bind in `headlamp.yaml`. Never bind the file. |
| `compose: services.k3s-server.command.N must be a string` | `--kube-apiserver-arg=oidc-username-prefix=oidc:` ends in a colon, so YAML reads it as a mapping key. | Quote it. Both `*-prefix=oidc:` args are quoted in `k3s.yaml` for this reason. |
| UI loads, every resource `Forbidden` | Authenticated but not authorised, or the RBAC subject does not match the real claim. | `docker logs k3s-server 2>&1 \| grep -iE 'forbidden\|unable to authenticate'` shows the identity the apiserver saw. |
| Headlamp looks healthy but lists no clusters | A missing or unreadable kubeconfig is logged, not fatal. | `docker logs headlamp \| grep 'kubeconfig not found'`, then [§4.2](#42-build-the-kubeconfig). |
| TLS error against `k3s-server:6443` | `k3s-server-data` was wiped, so the CA changed. | Redo [§4.2](#42-build-the-kubeconfig) and [§4.3](#43-grant-rbac). |
| ACME validation 404s on a domain | Extra stale `A` records on the apex send validators to a host that knows nothing of the challenge. | `dig +short nicolapasqualini.it A` should return only this machine. |
| `k3s-server` dies on `failed to evacuate root cgroup` | It was started by the orchestrator instead of `bin/start_k3s.sh`. | [§8](#8-loader-gaps-and-what-they-cost-here). |

If `auth.nicolapasqualini.it` ever stops resolving from inside a container,
note that k3s runs under real compose and can take `extra_hosts`; Headlamp,
being orchestrator-managed, cannot.

---

## 8. Loader gaps, and what they cost here

Two things about this setup look wrong and are not: the kubeconfig is written by
hand, and k3s sits in `containers/standalone/` outside the orchestrator. Both are
workarounds for unimplemented compose features, tracked in [TODO.md](../TODO.md).

| Gap | TODO.md | What it forces here | What to delete when it lands |
|---|---|---|---|
| **`${VAR}` interpolation** | [line 88](../TODO.md#L88), *Main functionalities* | The kanidm client secret cannot reach a compose file, so the kubeconfig is hand-built into `secrets/headlamp/config` | [§4.2](#42-build-the-kubeconfig) entirely, plus the TODO at [line 249](../TODO.md#L249) |
| **`privileged` / `tmpfs` / `ulimits`** | [line 154](../TODO.md#L154), *Nice to have* | k3s cannot boot under the orchestrator at all; it lives in `containers/standalone/` behind `bin/start_k3s.sh` | `bin/start_k3s.sh`, `containers/standalone/`, and the second command in [§5](#5-routine-start-and-stop) — a `git mv` back |
| **Network `external:`** | [line 160](../TODO.md#L160) | `NetworkConfig` reads only `driver`, so moving `k3s.yaml` back would make the orchestrator re-create `etcd-backend` rather than join it | Blocks the row above — see the note below |
| **`healthcheck` + boot gate** | [line 72](../TODO.md#L72) | Headlamp is reported started even when it cannot read its kubeconfig, and nothing can gate on it being genuinely ready | The "expected on first boot" caveat in [§4](#4-first-time-initialisation) |
| **`container_name` ignored** | [line 157](../TODO.md#L157), `docker.rs:980` | HAProxy must target the service key `headlamp:4466`; `container_name:` is decorative | The warning in [§2](#2-moving-parts) |
| **`extra_hosts`** | [line 152](../TODO.md#L152) | No in-compose fallback if kanidm's public name stops resolving from a container | The closing caveat in [§7](#7-troubleshooting) |

**Moving k3s back needs two gaps closed, not one.** `privileged`/`tmpfs`/`ulimits`
is the obvious blocker, but `k3s.yaml` also declares `etcd-backend` and
`ninoverse-network` as `external: true`. The orchestrator reads only `driver`
from a network definition, so it would try to create both a second time —
duplicate resource names across files being itself an open edge case
([line 191](../TODO.md#L191)). Close `privileged` alone and the move will appear
to work while quietly duplicating networks.
