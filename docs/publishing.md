# Crate Publishing & Releases

**English** | [简体中文](zh/publishing.md) · [Back to Overview](../README.md) · [Architecture](architecture.md) · [AGENTS.md](../AGENTS.md)

## Release Policy & Compatibility

- Source repository: <https://github.com/wsafight/wings>, licensed under `Apache-2.0` (see [LICENSE](../LICENSE)) and published exclusively to crates.io.
- **No Backward Compatibility in `0.x` / `0.0.x`**: Early releases may introduce breaking changes to syntax, ABI layouts, artifact manifests, and Rust APIs without deprecation shims.
- Because an unrelated third-party crate named `wings` already exists on crates.io, Wings publishes the 10 explicit workspace crates listed below rather than an umbrella `wings` or `wings-lang` package.

## Branch Protection & Manual Release Workflow

- Daily pull requests use the `incremental` CI check. Repository administrators must update protected-branch required checks from the old native/MSRV/site jobs to `incremental` when adopting this workflow; editing workflow files does not update GitHub branch protection.
- Before publishing, manually dispatch `.github/workflows/release.yml` on the exact release commit and require successful `native (macos-15, arm64)`, `native (ubuntu-24.04, x86_64)`, `msrv` (Rust `1.85.0`), and `site` jobs. Run the separate manual `benchmark.yml` when validating performance claims. Incremental cached results never replace release evidence.
- Releases are executed manually from a clean `main` checkout only when explicitly requested by a maintainer. Because crates.io versions are immutable, every crate is verified with `--dry-run` before publishing.

## Topological Publish Order

Internal dependencies specify both `path` and exact `version` constraints, requiring publication in topological DAG order via `cargo publish -p <crate> --locked --registry crates-io` (set `CARGO_REGISTRIES_CRATES_IO_INDEX=sparse+https://index.crates.io/` when a local mirror index is configured):

1. `wings-source`
2. `wings-syntax`
3. `wings-sema`
4. `wings-abi`
5. `wings-ir`
6. `wings-artifact`
7. `wings-codegen`
8. `wings-compile`
9. `wings-lsp`
10. `wings-vm`

## Release History

- **`0.0.3` (2026-11-22)**: Full workspace upgrade across all 10 crates featuring modern `wings-lang` v2 syntax, precise CFG reference roots & bounded recursion, O(1) `slot + generation` addressing, lock-free fast mailboxes, M:N preemptive `Scheduler`, `send_after` / `monitor`, multi-actor `build-app`, and `wings-vm serve` (`stdio` / `--tcp` / `--http`). Install via:
  `cargo install wings-compile wings-vm wings-lsp --version 0.0.3 --locked`
- **`0.0.2` (2026-11-12)**: First full publication of all 10 workspace crates (commit `58dc907`, [GitHub Release v0.0.2](https://github.com/wsafight/wings/releases/tag/v0.0.2)), covering the initial frontend, `wings-lsp`, and single-actor scalar AOT prototype.
- **`0.0.1` (2026-11-12)**: Initial publication of the 5 frontend and language-server crates (`wings-source`, `wings-syntax`, `wings-sema`, `wings-compile`, `wings-lsp`, commit `247cf72`, [GitHub Release v0.0.1](https://github.com/wsafight/wings/releases/tag/v0.0.1)).
