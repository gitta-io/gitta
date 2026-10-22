# Contributing to gitta

Thank you for helping. Bug reports, fixes and new language support are all welcome.

## Report a bug

Open an issue and use the bug report template. Tell us the gitta version (`gitta --version`), your operating system, the agents you run and the commands that show the problem. A small public repository that shows it is the best report of all.

Report security problems privately. See [SECURITY.md](SECURITY.md).

## Build and test

You need Rust (stable) and Git. Each crate builds on its own:

```sh
cargo build --release --manifest-path gitta-cli/Cargo.toml
cargo test --release --manifest-path gitta-core/Cargo.toml
cargo test --release --manifest-path gitta-cli/Cargo.toml
```

The end-to-end tests make real Git repositories in a temporary folder, so set a Git user name and email first if your machine has none.

`gitta-fs` and `gitta-sandbox` build on Linux only. `gitta-fs` needs the FUSE 3 development files (`libfuse3-dev` on Debian and Ubuntu).

## Send a change

1. Fork the repository and make a branch from `main`.
2. Keep each pull request to one change. Add a test that fails without your change and passes with it.
3. Run the tests above. CI runs them again on macOS and Linux.
4. When you add or change a dependency that does cryptography, update `sbom/crypto-inventory.json`. `python3 sbom/cbom.py check sbom/crypto-inventory.json` tells you when it is out of date.
5. Open the pull request and say what the change does and why.

New dependencies must use a licence that works with Apache 2.0, for example MIT, BSD, ISC or Apache 2.0.

## Write for people

Messages that gitta shows, and the docs in `docs/pages`, use short sentences, active voice and common words. Say what happened and what the reader can do next. Write the name as "gitta", in lowercase.

## Licence

By sending a contribution, you agree that it is licensed under the [Apache License 2.0](LICENSE), as section 5 of that licence says.

Everyone who takes part follows our [Code of Conduct](CODE_OF_CONDUCT.md).
