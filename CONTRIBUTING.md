# Contributing

Thanks for helping. Bug reports, benchmark results from your own fleet, and pull requests are all welcome.

## Build and test

```sh
cargo build --release -p prequal-epp -p prequal-router
tools/with-cmake.sh cargo test --workspace --all-features   # the Pingora crates need cmake
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all
```

The libraries build on Rust 1.85; `prequal-epp`, `prequal-router` and `prequal-pingora`'s `kubernetes` feature need
1.89. CI runs all of the above plus MSRV checks, docs, and both Docker images.

## Benchmarks

[docs/benchmarks.md](docs/benchmarks.md) has every reproduction command. For routing work the virtual-time simulator
is the fast loop: it runs the real scheduler against modelled engines, many seeds in about a minute.

```sh
cargo build --release -p prequal-testbed
POLICIES="prequal llmd-optimized" SEEDS="1 2 3" tools/vsim-compete.sh results/vsim
node tools/llm-compare.mjs results/vsim results/vsim prequal llmd-optimized
```

## Conventions

- Clippy clean at default lints, `cargo fmt` applied (see `rustfmt.toml`).
- No source file over 300 lines; split by responsibility.
- Behaviour changes come with tests.
- **Routing changes must show simulator results against `llmd-optimized`**: paste the `llm-compare.mjs` win/loss/tie
  table for every scenario into the pull request, including `SCENARIOS="cache cache-two-routers"`, and the
  production-trace scenarios if the change touches prefix keys or cold placement. A change that wins one scenario
  and loses another needs a reason.
- Comments explain what the code can't: non-obvious logic, gotchas, and why an alternative lost in measurement.

## Releasing

1. Bump `version` in the workspace `Cargo.toml` and `version`/`appVersion` in `deploy/helm/prequal-router/Chart.yaml`
   (plus image tags in the docs), and move `CHANGELOG.md`'s Unreleased entries under the new version.
2. Tag and push: `git tag v0.1.0 && git push origin v0.1.0`. `.github/workflows/release.yml` checks the versions
   match, publishes both images and the chart to `ghcr.io/<owner>`, and drafts a GitHub release with prebuilt binaries to review and
   publish.
3. Make the ghcr.io packages public (first release only: package settings → visibility).
4. Publish the crates, dependencies first:
   `for c in prequal-core prequal-server prequal-tower prequal-pingora prequal-llm prequal-epp prequal-router; do cargo publish -p $c; done`

## License

Unless you state otherwise, any contribution you intentionally submit for inclusion is dual-licensed under MIT or
Apache-2.0, without additional terms.
