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

Expected direct runtime dependencies are limited to:

- `tokio`
- `axum`
- `serde`
- `serde_json`
- `tracing`
- `tracing-subscriber`
- `thiserror`
- one small stream adapter only if Axum SSE cannot be expressed cleanly without it

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

Current proc macros are `thiserror-impl` and `tokio-macros`, plus their `proc-macro2`/`quote`/`syn` parsing stack. Serde is not present in Phase 1. These macros save substantial boilerplate but remain executable build inputs and are included in the lockfile review.

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

## Current status

Phase 1 has four exact direct dependencies and 17 crates.io packages in `Cargo.lock`. All registry entries have checksums. There are no Git sources, alternate registries, duplicate versions, native `links` packages, or frontend packages.

Licenses are MIT, Apache-2.0, and Unicode-3.0 combinations. The build scripts for `proc-macro2 1.0.107`, `quote 1.0.47`, and `thiserror 2.0.20` were inspected. They query/probe the configured Rust compiler and write only beneath Cargo's output directory; none performs a download. Proc macros are `thiserror-impl 2.0.20` and `tokio-macros 2.7.2`.

`cargo-audit 0.22.2` was pinned and installed under `/tmp/meld-tools`, not into the project or global toolchain. Against a RustSec database containing 1,225 advisories, it scanned the Phase 1 lockfile successfully and reported no vulnerabilities.

`cargo-deny` and `cargo-vet` are not installed. Licenses, sources, duplicates, native links, features, proc macros, unsafe-code presence, and build targets were inspected directly. Automated deny/vet policy remains post-Phase-1 hardening.
