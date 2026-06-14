# Implementation Plan: trim smolvm to "OCI → VM for coding agents"

**Audience:** an automated coding agent (Sonnet/Haiku-class) executing one phase
at a time. Each phase is mechanical, compiler-driven, ends with a green build
and a commit, and has explicit, runnable **success criteria**.

**Fork goal:** keep exactly one capability — turn an OCI image into a full
microVM on macOS + Linux, with the coding-agent restriction flags (`--net` off
by default, `--allow-host`, `--ssh-agent`, `--secret-env`/`--secret-file`,
volume mounts, `--cpus`/`--mem`) and the declarative `Smolfile`. Remove the
fleet HTTP API (`serve`), the binary packer (`pack`), and the Node SDK.

---

## Ground rules for the agent (read first)

1. **Work phase by phase, in order.** Do not start a phase until the previous
   phase's success criteria all pass. Commit at the end of each phase.
2. **Compiler-driven removal.** Delete a module/file, then run `cargo build` and
   let the errors point you at every remaining reference. Fix those references
   (usually: delete the line/import). Repeat until green.
3. **Do NOT touch the libkrun / libkrunfw fork.** The forks
   (`libkrun/`, `libkrunfw/` submodules, `nix/libkrun.nix`, `nix/libkrunfw.nix`)
   are **mandatory** — the kept features depend on fork-only libkrun symbols
   (`krun_set_egress_policy`/`krun_get_egress_handle` → `--allow-host`,
   `krun_add_net_unixstream` → `--net`, `krun_create_disk_overlay` → exec/stop
   persistence). libkrunfw is the embedded guest kernel. Leave all of it alone.
4. **Do NOT refactor core code.** Only delete leaf features and fix the resulting
   references. No renaming, no restructuring of `agent/`, `vm/`, `network/`,
   `registry/`, `data/`, `db/`, `config.rs`, `process.rs`, `secrets.rs`.
5. **Never delete something you haven't proven is unreferenced.** When a step
   says "delete file X", first run the grep gate it lists; if the grep finds a
   reference from a *kept* module, stop and report instead of guessing.
6. **One concern per commit.** If a phase says to remove dependencies, do that in
   its own commit after the code removal commit is green.

### How to build / verify (host-independent)

On **Linux**, `build.rs` loads libkrun at runtime via `dlopen` and does **not**
link it at build time, so the project compiles **without** libkrun installed.
These are valid gates on a plain Linux CI box:

```bash
cargo build --release            # must exit 0
cargo clippy --all-targets       # must produce no new warnings
cargo test --workspace           # must pass (skip VM-runtime tests, see below)
./target/release/smolvm --help   # inspect the command list
```

Runtime smoke tests (`smolvm machine run …`) require a VM-capable host
(`/dev/kvm` on Linux, Hypervisor.framework on macOS) **and** the bundled
libkrun/libkrunfw libraries. Treat those as **soft gates**: run them only "if a
VM-capable host is available," and never block a phase on them in CI. The hard
gates are: compiles, clippy clean, grep gates empty, `--help` shows the expected
command set, workspace tests pass.

---

## Phase 0 — Baseline (no code changes)

**Goal:** capture before/after metrics and confirm the starting point is green.

**Steps**
1. `cargo build --release` and record wall-clock time.
2. Record stripped binary size: `ls -l target/release/smolvm`.
3. Record dependency count: `cargo tree --edges normal | wc -l` (save full output
   to `/tmp/cargo-tree-before.txt`).
4. Record LOC: `find src crates -name '*.rs' | xargs wc -l | tail -1`.

**Success criteria**
- [ ] `cargo build --release` exits 0.
- [ ] `./target/release/smolvm --help` lists `machine`, `serve`, `pack`,
      `config` (the starting state).
- [ ] Baseline numbers saved (paste them into the Phase 0 commit message or a
      scratch file). No code committed in this phase.

---

## Phase 1 — Remove the fleet HTTP API (`serve`)

**Goal:** delete the entire HTTP/mTLS control plane and its OpenAPI surface.

**Why first:** `serve` (`src/api/`) is a top-level consumer; removing it first
avoids fighting its coupling to `embedded` and `pack` later.

**Steps**
1. Delete directories/files:
   - `src/api/` (whole directory)
   - `src/cli/serve.rs`, `src/cli/serve_tls.rs`, `src/cli/openapi.rs`
   - **Do NOT delete `src/cli/proxy_opts.rs`** — despite appearing in the
     original delete list, `ProxyOpts` is used by `machine.rs` and `pack.rs`
     for `--proxy`/`--no-proxy` image-pull flags. It is shared infrastructure,
     not serve-only. Verified: `grep -rn "proxy_opts\|ProxyOpts"
     src/cli/machine.rs src/cli/pack.rs` shows active usage.
2. Delete serve-only top-level modules **after** confirming they're unreferenced
   by kept code (run the grep gate below for each before deleting):
   - `src/log_rotation.rs` (safe to delete — only `src/lib.rs` pub mod declaration)
   - **Do NOT delete `src/systemd_scope.rs`** — referenced by
     `src/agent/manager.rs` (`crate::systemd_scope::adopt_into_scope`).
   - **Do NOT delete `src/dns_filter_listener.rs`** — referenced by
     `src/cli/internal_boot.rs` (`smolvm::dns_filter_listener::start`).
   - Grep gate per file, e.g.:
     `grep -rn "systemd_scope\|log_rotation\|dns_filter_listener" src --include=*.rs | grep -v -E "src/(api/|cli/serve|systemd_scope|log_rotation|dns_filter_listener)"`
     If this prints a reference from `agent/`, `vm/`, `cli/machine.rs`, or
     `cli/vm_common.rs`, **do not delete that file** — leave it and note it.
3. Edit `src/main.rs`: remove `mod`/`use` for the deleted CLI modules, remove the
   `Serve(cli::serve::ServeCmd)` enum variant, and its `Commands::Serve(cmd) =>
   cmd.run()` match arm.
4. Edit `src/cli/mod.rs`: remove `pub mod serve; pub mod serve_tls; pub mod
   openapi;` (keep `pub mod proxy_opts;` — shared with machine and pack).
5. Edit `src/lib.rs`: remove `pub mod api;`, `pub mod log_rotation;`, and the
   `pub use api::ApiDoc;` re-export.
6. Run `cargo build` and fix every remaining reference the compiler reports
   (these will be `crate::api::…` / `smolvm::api::…` / `ApiDoc` usages). Expect a
   few in `src/cli/` and possibly `data/`.
7. Remove now-dead OpenAPI derives from **kept** core types: search
   `grep -rn "utoipa\|ToSchema\|IntoParams\|utoipa::path" src` and delete those
   derives/attributes/imports (e.g. in `src/network/backend.rs`). Re-run
   `cargo build`.

**Dependency cleanup (separate commit, after code is green)**
Remove these `[dependencies]` lines from `Cargo.toml`, then `cargo build`. If the
build fails because one is still used, restore only that line:
`axum`, `axum-server`, `rustls`, `rustls-pemfile`, `rustls-pki-types`,
`tower-http`, `tokio-stream`, `async-stream`, `utoipa`, `utoipa-axum`,
`utoipa-swagger-ui`, `metrics-exporter-prometheus`.
Verify-before-removing (may still be used by kept code — grep first, keep if
referenced): `metrics` (used in `agent/manager.rs`), `parking_lot`, `ipnet`,
`futures-util` (kept — registry), `tokio` (kept — registry/agent; do **not**
remove, optionally narrow `features = ["full"]` only if `cargo build` still
passes). Prefer `cargo machete` / `cargo +nightly udeps` if available to confirm.

**Success criteria**
- [ ] `cargo build --release` exits 0.
- [ ] `cargo clippy --all-targets` produces no new warnings.
- [ ] `cargo test --workspace` passes.
- [ ] `./target/release/smolvm --help` lists `machine`, `pack`, `config` and
      **does NOT** list `serve`.
- [ ] `./target/release/smolvm serve --help` exits non-zero (unknown subcommand).
- [ ] Grep gate empty: `grep -rn "crate::api\|smolvm::api\|cli::serve\|ApiDoc\|utoipa" src` returns nothing.
- [ ] `git status` shows `src/api/` and the four serve CLI files deleted.
- [ ] Commit: `feat: remove fleet HTTP API (serve)` (+ a follow-up
      `chore: drop serve-only dependencies`).

---

## Phase 2 — Remove the packer (`pack`)

**Goal:** delete `.smolmachine` packaging and the packed-binary run path.

**Steps**
1. Delete:
   - `crates/smolvm-pack/` (whole crate)
   - `src/cli/pack.rs`, `src/cli/pack_run.rs`
2. Edit `Cargo.toml`:
   - Remove `"crates/smolvm-pack"` from `[workspace].members`.
   - Remove the `smolvm-pack = { path = … }` line from `[dependencies]`.
3. Edit `src/main.rs`:
   - Remove the `Pack(cli::pack::PackCmd)` enum variant and its match arm.
   - Remove the packed-binary fast path at the top of `main()`:
     the `if let Some(mode) = smolvm_pack::detect_packed_mode() {
     cli::pack_run::run_as_packed_binary(mode); }` block and the `smolvm_pack`
     import.
4. Edit `src/cli/mod.rs`: remove `pub mod pack; pub mod pack_run;`.
5. Remove `--from <FILE.smolmachine>` support in `machine create`: search
   `grep -rn "from\|smolmachine\|smolvm_pack\|smolvm-pack" src/cli/machine.rs src/cli/vm_common.rs`
   and delete the pack/extract-backed `--from` flag and its handling. (The rest
   of `machine create` — image-based and bare VMs — stays.)
6. Edit `build.rs`: the packed-section placeholder logic (the `OUT_DIR`
   `smolvm_placeholder.bin` block around lines 70-88) exists only for `pack
   --single-file`. Remove it **only if** `cargo build` stays green afterward;
   if unsure, leave it (it's inert) and note it.
7. `cargo build` and fix remaining references the compiler reports.

**Success criteria**
- [ ] `cargo build --release` exits 0.
- [ ] `cargo clippy --all-targets` produces no new warnings.
- [ ] `cargo test --workspace` passes.
- [ ] `./target/release/smolvm --help` lists `machine`, `config` and does **NOT**
      list `pack`.
- [ ] `./target/release/smolvm pack --help` exits non-zero.
- [ ] Grep gate empty: `grep -rn "smolvm_pack\|smolvm-pack\|pack_run\|detect_packed_mode\|smolmachine" src Cargo.toml` returns nothing.
- [ ] `crates/smolvm-pack/` no longer exists; not in `[workspace].members`.
- [ ] Soft gate (if VM host): `smolvm machine create --name t --image alpine &&
      smolvm machine start --name t && smolvm machine exec --name t -- true &&
      smolvm machine delete --name t` succeeds.
- [ ] Commit: `feat: remove .smolmachine packer (pack)`.

---

## Phase 3 — Remove the Node SDK and the `embedded` facade

**Goal:** delete the language SDK and the embedded-runtime facade it sits on.
`embedded` is a thin wrapper over `agent::AgentManager`; the CLI uses
`AgentManager` directly, so nothing in the `machine` path needs it once `api`
and `pack` (its only other consumers) are gone.

**Steps**
1. **Precondition check.** Confirm `embedded` is now only used by itself + the
   SDK glue:
   `grep -rn "embedded::\|EmbeddedRuntime\|MachineSpec" src --include=*.rs | grep -v "src/embedded/"`
   Expect only comment hits (not code). If any **code** in a kept module still
   uses it, stop and report.
2. Delete:
   - `src/embedded/` (whole directory)
   - `sdks/` (whole directory — the TypeScript SDK)
   - `smolvm-sdk/` (empty submodule placeholder)
3. Edit `src/lib.rs`: remove `pub mod embedded;` and any
   `pub use embedded::…` re-exports.
4. Edit `.gitmodules`: remove the `[submodule "smolvm-sdk"]` block. Run
   `git rm --cached smolvm-sdk` if git still tracks it. Leave the `libkrun` and
   `libkrunfw` submodule entries untouched.
5. Edit `build.rs` / `Makefile.toml`: remove any SDK/embedded staging steps
   (search for `sdk`, `embedded`, `stage-embedded`). Remove `sdks/scripts/*`
   references. Keep all libkrun-related logic.
6. `cargo build` and fix remaining references.

**Success criteria**
- [ ] `cargo build --release` exits 0.
- [ ] `cargo clippy --all-targets` produces no new warnings.
- [ ] `cargo test --workspace` passes.
- [ ] Grep gate empty: `grep -rn "embedded::\|EmbeddedRuntime\|MachineSpec" src --include=*.rs` returns nothing.
- [ ] `src/embedded/`, `sdks/`, `smolvm-sdk/` no longer exist; `.gitmodules` has
      no `smolvm-sdk` entry but still has `libkrun` + `libkrunfw`.
- [ ] `./target/release/smolvm --help` unchanged from Phase 2 (`machine`,
      `config`).
- [ ] Commit: `feat: remove Node SDK and embedded runtime facade`.

---

## Phase 4 — (Optional) Make GPU build-time-only — NOT a code removal

**Decision:** **keep the GPU code.** It is ~365 lines, self-contained behind the
optional libkrun symbol `krun_set_gpu_options2` (which resolves to `None` and
no-ops when libkrun is built without `GPU=1`), plus the `--gpu`/`gpu_vram` flags
and Smolfile fields. Deleting it buys almost nothing and risks touching the core
launcher. Do **not** remove the Rust.

If you want a smaller **distribution** (not fewer lines), neutralize GPU at the
build/packaging layer only:
1. `nix/` / build config: build libkrun with `withGpu = false` (the flake
   currently sets `withGpu = final.stdenv.hostPlatform.isLinux`).
2. Stop bundling the GPU libraries: remove `lib/libvirglrenderer.1.dylib`,
   `lib/libepoxy.0.dylib`, `lib/libMoltenVK.dylib` from the shipped bundle and
   from any staging in `Makefile.toml` / `scripts/`.

With those two changes, `krun_set_gpu_options2` is absent → `--gpu` errors
cleanly → the GPU code path is dormant. No source changes required.

**Success criteria (only if this phase is attempted)**
- [ ] `cargo build --release` still exits 0 (no Rust changed).
- [ ] GPU libs no longer present in the packaged `lib/` output.
- [ ] Soft gate (if VM host): `smolvm machine run --image alpine -- true`
      succeeds; `smolvm machine run --gpu --image alpine -- true` fails with a
      clear "GPU not supported" style error rather than a crash.
- [ ] Commit: `chore: build without GPU support (drop bundled virgl/epoxy/moltenvk)`.

---

## Phase 5 — Docs, examples, tests, final metrics

**Goal:** make the repo's surface match the trimmed feature set and confirm the
wins landed.

**Steps**
1. `README.md` and `AGENTS.md`: delete the **Pack**, **serve/fleet**, and
   **SDK** sections and any pack/serve examples. Keep `machine`, `config`,
   `Smolfile`, GPU (if kept), and the comparison/platform tables.
2. `docs/`: delete `docs/lossless-serve-restart.md` (serve-only). Review the
   others for serve/pack references.
3. `examples/`: delete examples that use `pack` or `serve`; keep
   image/Smolfile-based ones (e.g. `python-app`, `node-app`). Grep
   `grep -rln "pack \|smolvm serve\|smolmachine" examples`.
4. Delete tests for removed features:
   `grep -rln "serve\|pack\|smolmachine\|EmbeddedRuntime\|api::" tests/` and
   remove those test files/cases.
5. Re-measure vs. Phase 0: build time, `ls -l target/release/smolvm`,
   `cargo tree --edges normal | wc -l` (diff against
   `/tmp/cargo-tree-before.txt`), and LOC. Put the before/after table in the
   commit message.

**Success criteria**
- [ ] `cargo build --release` exits 0; `cargo clippy --all-targets` clean;
      `cargo test --workspace` passes.
- [ ] `grep -rn "smolvm serve\|smolvm pack\|\.smolmachine\|SDK" README.md AGENTS.md`
      returns nothing (except an intentional "removed in this fork" note, if any).
- [ ] `./target/release/smolvm --help` lists exactly `machine` and `config`.
- [ ] Dependency count (`cargo tree | wc -l`) is meaningfully lower than the
      Phase 0 baseline (the serve deps are gone).
- [ ] Acceptance suite passes on a VM-capable host:
      ```bash
      smolvm machine run --image alpine -- uname -a
      smolvm machine run --net --image alpine --allow-host registry.npmjs.org \
        -- wget -q -O /dev/null https://registry.npmjs.org
      smolvm machine create --net --name dev --ssh-agent && \
        smolvm machine start --name dev && \
        smolvm machine exec --name dev -- true && \
        smolvm machine delete --name dev
      smolvm machine create --name dev2 -s Smolfile && \
        smolvm machine start --name dev2 && smolvm machine delete --name dev2
      ```
- [ ] Commit: `docs: trim README/AGENTS/examples to machine-only surface`.

---

## Expected outcome

- **Commands:** 4 → 2 (`machine`, `config`).
- **LOC removed:** ~18k of ~69k Rust (~26%), plus the 1.7k-line TS SDK. The
  microVM engine (guest agent, host orchestration, OCI pull, NAT/DNS egress,
  overlay persistence) stays — it's the irreducible core you want.
- **Biggest win:** ~12 heavyweight server/OpenAPI dependencies removed → smaller
  binary, faster compile, smaller attack surface.
- **libkrun/libkrunfw fork:** unchanged and still required (kept features depend
  on its custom symbols). Do not attempt to switch to upstream.

## Ordering rationale (why this sequence)

`serve` → `pack` → `SDK/embedded` removes consumers before the shared facade
they depend on, so the build stays green at every step. GPU is intentionally
last and optional because it's build-time-gated, not code-coupled. Docs/tests
come last so the agent isn't maintaining prose mid-refactor.
