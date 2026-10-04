## What and why

## Checklist

- [ ] `cargo fmt --all` and `cargo clippy --workspace --all-targets --all-features -- -D warnings` are clean
- [ ] `cargo test --workspace --all-features` passes; behaviour changes have tests
- [ ] No source file over 300 lines
- [ ] Routing changes: simulator win/loss/tie table vs `llmd-optimized` below (see [CONTRIBUTING](../CONTRIBUTING.md))

## Simulator results (routing changes only)
