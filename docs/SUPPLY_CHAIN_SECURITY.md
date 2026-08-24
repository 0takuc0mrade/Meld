# Rust Supply-Chain Security

## Objective

Meld's deterministic control plane is only trustworthy if the code compiled into it is understood and reproducible. Rust's package ecosystem is productive, but dependencies may run code at build time through `build.rs`, proc macros, native compilers, or code generators. A committed lockfile and an advisory scan are necessary controls, not complete protection.

This policy is intentionally practical for a three-day MVP. It establishes a review gate now and lists stronger measures for later.

## Threats considered

- typo-squatted or look-alike crate names;
- a compromised maintainer account publishing a malicious version;
- malicious or unexpected build scripts and proc macros;
- unpinned Git branches or revisions changing underneath a build;
- an unnecessary default feature pulling in networking, native code, or a large dependency tree;
- a vulnerable, yanked, abandoned, or unmaintained transitive crate;
- dependency confusion through alternate registries or source replacement;
- CI actions or install scripts fetched by mutable tag;
- secrets exposed to build scripts, tests, logs, or model-provider code;
- a lockfile changed without human review;
- a demo-day rebuild unexpectedly resolving or downloading different content.

## Trust boundaries

The most trusted code is Meld's domain state machine and supervisor. External model integration is less trusted and lives behind the `Worker` trait. Agent output is untrusted data even when its SDK dependency is trusted.

The build environment is also a trust boundary: Cargo build scripts and proc macros execute with the builder's permissions. Builds must not receive production secrets, broad cloud credentials, or writable access beyond what they need.

## Dependency acceptance checklist

Before adding a direct crate:

1. Confirm the exact official crate name and repository from crates.io and the upstream project.
2. Write down the capability that standard Rust or an existing dependency cannot reasonably provide.
3. Inspect recent release cadence, ownership changes, repository health, security history, and maintenance status.
4. Inspect enabled default features and disable unnecessary ones.
5. Compare `cargo tree -e features` before and after the change.
6. Inspect the full lockfile diff, including packages not named in `Cargo.toml`.
7. Identify proc-macro crates, `build.rs` targets, `links` declarations, native code, downloaded/generated artifacts, and unsafe code.
8. Reject unexpected Git sources, alternate registries, duplicate major versions, or native dependencies until explained.
9. Run formatting, lints, tests, advisory checks, source/license checks, and the real application path.
10. Record the decision and tradeoff in `BUILD_LOG.md` or `DECISIONS.md`.

No dependency should be added solely to avoid a few clear lines of application code.

## Initial dependency budget

Current direct runtime dependencies are limited to:

- `tokio`
- `axum`
- `serde`
- `tracing`
- `tracing-subscriber`
- `thiserror`
- `tokio-stream`

Rig and its one selected provider are a later, feature-gated review. No UUID crate is initially required; process-local typed counters are adequate for in-process IDs. No frontend packages are required.

“Expected” is not pre-approval. Exact versions and their transitive graphs must still be reviewed when the manifest is created.

## Manifest and lockfile policy

- Commit `Cargo.lock` because Meld is an application.
- Pin `rust-toolchain.toml` to the exact tested stable toolchain for the demo.
- Use Cargo's current workspace resolver.
- Prefer narrowly enabled crate features and `default-features = false` where practical.
- Use crates.io releases. Do not use a Git branch, tag, or raw URL dependency by default.
- If a Git dependency becomes unavoidable, document why, pin a full commit SHA, review that exact source, and plan its removal.
- Do not add an alternate registry or `[patch]` source without an explicit decision record.
- Treat every `Cargo.lock` change as security-sensitive review material.

Cargo.lock includes checksums for registry packages, which detects changed downloaded content. It does not prove that the originally published code is safe.

## Automated checks

Phase 1 runs a pinned, isolated `cargo-audit` against `Cargo.lock`. A later `deny.toml` and CI workflow should additionally cover:

- RustSec advisories and yanked crates;
- denied or unreviewed licenses;
- unexpected registries and Git sources;
- duplicate crate versions where they increase risk or size;
- unknown sources.

Proposed verification sequence after tools are deliberately installed or provided by CI:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo audit
cargo deny check
cargo tree --workspace --all-features -e features
```

Tool installation is itself a supply-chain action. Do not blindly run `cargo install` during the demo build. Pin tool versions in CI or use reviewed, checksum-verified binaries.

CI third-party actions must be pinned to immutable commit SHAs rather than mutable tags. Workflow permissions should be read-only unless a job demonstrably needs more.

## Manual build-script and proc-macro review

Before accepting the lockfile, use Cargo metadata to enumerate packages with custom build targets and proc-macro targets. For each one:

- identify why it is present;
- inspect its source and repository mapping;
- confirm it does not download code or binaries during build;
- note filesystem, environment, compiler, and native-tool assumptions;
- minimize the environment variables and credentials visible to the build.

Current proc macros are `thiserror-impl`, `tokio-macros`, and `serde_derive`, plus their `proc-macro2`/`quote`/`syn` parsing stack. Serde derives are restricted to HTTP/SSE DTOs; the domain is not serialization-coupled. These macros save substantial boilerplate but remain executable build inputs and are included in the lockfile review.

Native TLS and C-library dependencies should be avoided unless required. Prefer Rustls-backed provider features where the selected integration supports them, after verifying the exact feature graph.

## Rig integration gate

Before enabling Rig:

1. Confirm the exact version and official repository.
2. Add it in a separate change so its complete `Cargo.lock` delta is isolated.
3. Select exactly one provider and disable unused default/provider features.
4. Enumerate added build scripts, proc macros, native code, network stacks, and TLS choices.
5. Run advisory/source/license checks and the full test suite.
6. Confirm the core builds and tests with Rig's Cargo feature disabled.
7. Give the adapter only the provider credential it needs; never pass the complete environment into worker input or logs.
8. Apply server-side timeouts and output-size/schema validation around provider responses.

If the graph is too large, unstable, or unreviewable before demo day, keep the deterministic worker path for the demo. Reliability claims must not depend on an unsafe rush integration.

## Demo-day build and runtime procedure

1. Start from a clean, reviewed source revision and committed lockfile.
2. Fetch dependencies before the event on a trusted network.
3. Run all checks with `--locked`.
4. Build the exact release binary to be demonstrated.
5. Record the source revision, Rust version, binary checksum, and enabled Cargo features.
6. Re-run the smoke test using the release binary.
7. Avoid rebuilding or updating dependencies on the venue network.
8. Run with outbound network disabled for the deterministic fallback; for the Rig path, allow only the required provider endpoint where operationally possible.
9. Use a scoped, low-value demo credential and remove it after the event.
10. Never expose provider keys in command history, browser assets, tracing fields, screenshots, or SSE events.

The deterministic offline fallback must use the identical supervisor, deadline, reassignment, submission, and verifier path. Only the `Worker` implementation differs.

## Secret and agent-output handling

- Read provider credentials at runtime, not compile time.
- Do not embed `.env` files or keys into the binary or static assets.
- Redact authorization headers and avoid logging prompts/full outputs by default.
- Set maximum request and response sizes.
- Deserialize into typed schemas and validate before state mutation.
- Do not give workers shell, filesystem, arbitrary URL-fetching, or environment-enumeration authority in the MVP.
- Use fixed provider base URLs rather than accepting them from an untrusted task payload.

## Response to a suspicious dependency event

If a crate release, checksum, maintainer change, advisory, or unexplained lockfile delta looks suspicious:

1. Stop updating and preserve the known-good lockfile and build artifact.
2. Do not “test” suspicious build code on a machine containing credentials.
3. Identify whether the package was downloaded, built, tested, or executed and which credentials/files were exposed.
4. Quarantine affected build outputs and rotate any credential that may have been visible.
5. Pin or revert to a reviewed version through a normal manifest change; do not hand-edit registry contents.
6. Rebuild in an isolated environment from reviewed sources.
7. Document the finding and resolution in `BUILD_LOG.md`.

## Post-MVP hardening

- adopt `cargo-vet` with explicit audit criteria for high-risk crates;
- consider a reviewed vendored source tree or controlled internal registry;
- generate an SBOM for releases;
- add signed build provenance and immutable artifact retention;
- build in an isolated environment without secrets or general network access;
- add reproducibility checks where the toolchain and dependencies permit them;
- define a dependency update cadence instead of opportunistic updates;
- monitor RustSec and upstream security notices;
- document unsafe-code policy and inspect unsafe transitive crates;
- sign release checksums and retain the exact demo artifact.

## Phase 2 dependency review

The Phase 2 graph was resolved in `/tmp` before changing the project manifest. Two proposed conveniences were removed during review: Axum’s `query` feature and a direct `serde_json` entry. The final additions are:

| Direct crate | Exact version | Enabled features | Purpose |
| --- | --- | --- | --- |
| `axum` | 0.8.9 | `http1`, `json`, `tokio`, `tracing`; defaults off | Router, JSON responses, SSE, HTTP/1 server |
| `serde` | 1.0.229 | `derive` | Serialize transport DTOs |
| `tokio-stream` | 0.1.19 | `sync`; defaults off | Adapt the bounded SSE receiver into a stream |
| `tokio` (existing) | 1.53.1 | adds `net`, `signal` | TCP listener and Ctrl+C shutdown |

Dev-only direct entries are `tower 0.5.3` with only `util` and `http-body-util 0.1.5`; both are already required transitively by Axum and are named directly only so integration tests can call the router and inspect streaming body frames.

The complete lockfile contains 60 external crates, up from 17 in Phase 1: a 43-crate delta. All packages use `registry+https://github.com/rust-lang/crates.io-index` and have checksums. There are no Git dependencies, alternate registries, duplicate name/version pairs, native `links` declarations, native TLS stacks, frontend packages, or runtime downloads.

New build-script-bearing packages are `httparse 1.10.1`, `libc 0.2.189`, `serde 1.0.229`, `serde_core 1.0.229`, `serde_json 1.0.151`, and `zmij 1.0.23`; existing build scripts remain `proc-macro2`, `quote`, and `thiserror`. The only new proc macro is `serde_derive 1.0.229`. Metadata reports no native-linked package. The new build scripts were source-inspected: they emit Cargo compiler configuration, query the configured Rust/compiler/platform tool version, and in Serde’s case use Cargo’s `OUT_DIR`; no script contains a downloader or opens a network connection. `libc` has target-specific local probes such as `freebsd-version`/`emcc`, which are not used on this Linux target.

No JavaScript package manager, web framework package, analytics script, remote font, CDN, or externally loaded runtime asset exists. A Content Security Policy limits scripts, styles, images, and connections to the Rust server itself.

## Current status

Phase 2 has seven exact direct runtime dependencies and 60 external crates.io packages in `Cargo.lock`. The dependency delta, enabled features, build targets, proc macros, sources, duplicates, and native links were inspected before acceptance.

`cargo-audit 0.22.2` remains isolated under `/tmp/meld-tools`. It refreshed and loaded 1,225 RustSec advisories, scanned all 61 lockfile records, and returned no vulnerability finding. The separate crates.io yanked-version lookup timed out, so the successful recorded advisory scan was repeated explicitly with `--no-yanked`; yanked status is not claimed as verified. `cargo-deny` and `cargo-vet` are not installed. Automated deny/vet policy, criteria-based audits, vendoring, SBOM generation, and isolated reproducible release builders remain post-MVP hardening.
