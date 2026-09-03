# Release process

Only `lenso-capability-search` and `lenso-capability-search-index` are public
registry packages. The PostgreSQL implementation remains private to this
repository. The linked Agent Tool adapter is also private and is not part of
the registry release set.

Publication is manual-only from reviewed `main` through
`.github/workflows/release-plz.yml`. Repository pushes do not run release
automation. A live run requires `live=true`, the literal confirmation
`publish`, and `main`.

Before the first release, allocate both crate names on crates.io and configure
a separate Trusted Publisher for each:

- owner: `LioRael`
- repository: `lenso-search-plugin`
- workflow: `release-plz.yml`
- environment: unset

Only the confirmed live job receives `id-token: write`. The workflow has no
registry-token fallback.

## Required evidence

```sh
cargo fmt --all -- --check
cargo check --locked --workspace --all-targets
lenso-contract-codegen workspace check --manifest-path Cargo.toml
cargo test --locked --workspace -- --include-ignored --test-threads=1
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo package --locked -p lenso-capability-search
cargo package --locked -p lenso-capability-search-index
```

The ignored PostgreSQL acceptance tests require the disposable database
configured by CI. Generated Capability projections are locked artifacts and
must not be edited by hand.
