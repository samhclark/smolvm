# Fork Scope Analysis: "OCI → VM for coding agents" only

**Goal of the fork:** keep exactly one capability — turn an OCI image into a
full microVM on macOS and Linux, with the convenient restriction flags coding
agents need (`--net` off by default, `--allow-host`, `--ssh-agent`,
`--secret-env`/`--secret-file`, volume mounts, `--cpus`/`--mem`) and the
declarative `Smolfile`. Drop everything else.

This document answers two questions:
1. Does cutting the other features meaningfully reduce scope and lines of code?
2. What's the plan to do it (rip-out vs. rewrite-from-scratch)?

---

## 1. Does it meaningfully reduce scope and LOC?

**Scope: yes, substantially.** **LOC: moderately (~25–30%), not dramatically.**

The headline: the "OCI → microVM for yolo coding agents" capability *is* the
hard core of this project. The features you don't want are real, separable, and
worth a lot of dependency/complexity — but the irreducible engine (guest agent,
host orchestration, OCI pull, networking, libkrun integration) stays, and that's
the bulk of the code.

### Current size

| Area | LOC (Rust) |
|------|-----------:|
| `src/` (host CLI + library) | 38,631 |
| `crates/` (workspace crates) | 30,407 |
| **Total Rust** | **69,038** |
| `sdks/node` (TypeScript SDK) | 1,748 |

### The four top-level CLI commands today

```
smolvm machine …   ← KEEP (this is what you want)
smolvm serve …     ← DROP (HTTP/fleet API server)
smolvm pack …      ← DROP (portable .smolmachine executables)
smolvm config …    ← KEEP (registry auth / defaults)
```

Plus a Node SDK (`sdks/node`) backed by an embedded-runtime facade
(`src/embedded`), and optional GPU passthrough.

### What's removable, and what it costs the project

| Feature to drop | Code | Approx. LOC | Notes |
|---|---|---:|---|
| **Fleet / HTTP API (`serve`)** | `src/api/**` | 5,624 | mTLS control plane, supervisor, OpenAPI |
| | `src/cli/serve.rs`, `serve_tls.rs`, `openapi.rs`, `proxy_opts.rs` | 845 | |
| | `src/systemd_scope.rs`, `src/log_rotation.rs`, `src/dns_filter_listener.rs` | 482 | lossless-restart plumbing |
| **Packer (`pack`)** | `src/cli/pack.rs`, `src/cli/pack_run.rs` | 3,650 | |
| | `crates/smolvm-pack/**` | 6,720 | `.smolmachine` format, packer, extract |
| **Node SDK / embedded** | `src/embedded/**` | 846 | thin facade over `AgentManager` |
| | `sdks/node/**` (TS) | 1,748 | separate package |
| **GPU passthrough** (optional) | scattered in `agent/`, `process.rs` | ~365 | virtio-gpu/Venus wiring |
| **Removable Rust subtotal** | | **~18,200** | **~26% of Rust LOC** |

### What must stay (the irreducible core — ~50k LOC)

| Component | LOC | Why it's required |
|---|---:|---|
| `crates/smolvm-agent` (in-guest agent) | 13,671 | runs inside the VM; exec/files/net setup |
| `src/agent` (host-side manager/client/launcher) | 7,128 | boots libkrun, vsock control channel |
| `crates/smolvm-network` | 4,972 | NAT, DNS filtering = `--allow-host` |
| `crates/smolvm-registry` | 2,470 | OCI image pull (the "OCI" in OCI→VM) |
| `crates/smolvm-protocol` | 1,987 | host↔guest wire protocol |
| `crates/smolvm-smolfile` | 587 | the declarative `Smolfile` you want |
| `src/cli/machine.rs` + `vm_common.rs` + parsers/internal_boot | ~7,000 | the `machine` command + all restriction flags |
| `src/vm`, `network`, `platform`, `data`, `db`, `config`, `process`, `secrets`, `storage`, `registry`, `disk_utils`, `settings`, `dns_filter` | ~10,000 | VM lifecycle, persistence, secrets, NAT/DNS |

**Conclusion.** You collapse 3 of 4 top-level commands into 1, delete an entire
HTTP/mTLS control plane, a binary packer, and a language SDK. Conceptually the
project gets *much* smaller and easier to reason about. But because the
microVM engine is inherently complex, the line count drops by only about a
quarter. The remaining ~50k LOC is genuinely the thing you want to keep.

### The bigger win is the dependency tree, not LOC

Dropping `serve` (and the OpenAPI surface) lets you delete a large set of
heavyweight dependencies that exist *only* for the server:

`axum`, `axum-server`, `rustls`, `rustls-pemfile`, `rustls-pki-types`,
`tower-http`, `tokio-stream`, `async-stream`, `utoipa`, `utoipa-axum`,
`utoipa-swagger-ui`, `metrics-exporter-prometheus`, `ipnet`.

`tokio` and `futures-util` stay (the OCI registry client is async), but you can
drop `tokio`'s `"full"` feature down to what the registry/agent actually use.
This is where you get real wins: **smaller binary, faster compile, smaller
attack surface** — which matters more for a "yolo coding agent sandbox" than raw
LOC.

---

## 2. Recommendation: rip out, don't rewrite

**Rewrite-from-scratch is the wrong call here.** The parts that are hard to
build correctly — the in-guest agent, the libkrun launch/boot path, the vsock
protocol, NAT + DNS egress filtering (`--allow-host`), OCI pull, and the
overlay/persistence model — are *exactly* the parts you want to keep. A rewrite
would mean re-deriving ~50k LOC of subtle, platform-specific systems code for
zero functional gain. Forking and deleting is lower-risk and far faster.

So: fork, then surgically remove `serve`, `pack`, the SDK, and (optionally) GPU.

### Phased removal plan

Each phase is independently compilable and testable. Do them in this order so
the build stays green.

**Phase 0 — Fork & baseline**
- Fork the repo; create the trimmed branch.
- Record a baseline: `cargo build --release` time, stripped binary size,
  `cargo tree` output. You'll compare against these at the end.
- Confirm `smolvm machine run --net --image alpine -- uname -a` works on your
  target host(s).

**Phase 1 — Remove the Node SDK / embedded facade (lowest risk)**
- Delete `sdks/`, `smolvm-sdk/`.
- Delete `src/embedded/` and its `pub mod embedded;` in `src/lib.rs` plus the
  `MachineSpec`/`EmbeddedRuntime` re-exports.
- The CLI uses `agent::AgentManager` directly, so nothing in the `machine` path
  breaks. Fix the few `pack`/`api` references (they're removed in later phases
  anyway).
- Remove SDK staging from `build.rs` / `Makefile.toml` / `sdks/scripts`.
- `cargo build` must pass.

**Phase 2 — Remove the packer (`pack`)**
- Delete `crates/smolvm-pack/` and drop it from `[workspace].members` and
  `[dependencies]` in `Cargo.toml`.
- Delete `src/cli/pack.rs`, `src/cli/pack_run.rs`.
- In `src/main.rs`: remove the `Pack` subcommand, the `detect_packed_mode()` /
  `run_as_packed_binary()` fast-path at the top of `main()`, and related imports.
- Remove `--from FILE.smolmachine` handling in `machine create` (it depends on
  the pack/extract format). Verify with `smolvm machine create` from an image.
- Remove pack examples/docs references.
- `cargo build` must pass.

**Phase 3 — Remove the fleet HTTP API (`serve`)**
- Delete `src/api/`, `src/cli/serve.rs`, `src/cli/serve_tls.rs`,
  `src/cli/openapi.rs`, `src/cli/proxy_opts.rs`.
- Delete serve-only plumbing: `src/systemd_scope.rs`, `src/log_rotation.rs`,
  `src/dns_filter_listener.rs` (verify each isn't referenced by the `machine`
  path first — `log_rotation` in particular may need a quick grep).
- In `src/main.rs`: remove the `Serve` subcommand. In `src/lib.rs`: remove
  `pub mod api;` and the `ApiDoc` re-export.
- Remove the now-dead OpenAPI `utoipa` derives sprinkled in core types
  (e.g. `src/network/backend.rs`).
- Delete serve-only deps from `Cargo.toml` (see list above) and trim `tokio`
  features. Run `cargo build`, then chase any remaining compile errors —
  they pinpoint the last couplings.
- Delete `docs/lossless-serve-restart.md` and serve references in README/AGENTS.

**Phase 4 — (Optional) Remove GPU passthrough**
- Only if you don't want `--gpu`. It's ~365 lines woven through `agent/launcher`,
  `agent/manager`, `process.rs`. Remove the `--gpu`/`gpu_vram` flags, the
  `krun_set_gpu_options*` calls, and the Smolfile `gpu` fields. Lower priority —
  it's small and self-contained.

**Phase 5 — Cleanup & docs**
- Rewrite `README.md` and `AGENTS.md` down to the `machine` + `config` +
  `Smolfile` surface (delete pack/serve/SDK sections).
- Prune `examples/` to the ones that don't use pack/serve.
- `cargo clippy --all-targets` and the remaining tests; delete tests for removed
  features.
- Re-measure binary size / compile time / `cargo tree` vs. the Phase 0 baseline
  and confirm the dependency win landed.

### Acceptance check (the fork still does what you want)

```bash
# OCI → ephemeral VM, no network
smolvm machine run --image alpine -- uname -a
# network opt-in + egress allowlist
smolvm machine run --net --image alpine --allow-host registry.npmjs.org \
  -- wget -q -O /dev/null https://registry.npmjs.org
# persistent dev machine + ssh-agent forwarding + secrets
smolvm machine create --net --name dev --ssh-agent
smolvm machine start --name dev
smolvm machine exec --name dev --secret-env OPENAI_API_KEY=OPENAI_API_KEY -- env
# declarative config
smolvm machine create --name dev2 -s Smolfile && smolvm machine start --name dev2
```

### Rough effort

- Phases 1–3: about 1–2 focused days for someone comfortable in Rust; the
  compiler does most of the work of finding couplings once the modules are
  deleted and the `Cargo.toml` deps are pulled.
- Phase 4 (GPU): a few hours, optional.
- Phase 5 (docs/examples/tests): half a day.

Maintaining the fork long-term means rebasing on upstream `machine`/agent
changes — easy, since you only deleted leaf features and didn't refactor the
core.
