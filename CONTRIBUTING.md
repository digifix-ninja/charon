# Contributing to Charon

Thanks for helping the ferryman. Bug reports, ideas and pull requests are all welcome.

## Reporting a bug

Open an [issue](../../issues/new/choose) with:

- your system and Charon's version (in **About**),
- the job's mode and trigger (mirror, backup or two-way; manual, on change, …),
- what you expected and what happened,
- the relevant lines from the job's **Activity** log, if any.

Please leave out personal folder names you don't want public.

## Suggesting a feature

Open an issue describing the problem you'd like solved, not only the solution. For bigger
changes, please discuss them in an issue before writing code.

## Pull requests

1. Fork the repository and create a branch from `main`.
2. Follow the code style: small objects with one job each, a short comment on every function,
   and names that read like plain English. See [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) for the
   project layout.
3. Run the checks:
   ```sh
   cargo fmt --all
   cargo clippy --workspace -- -D warnings
   cargo test --workspace
   ```
4. Anything that changes how files are copied or deleted needs a test in `core/`
   (`core/tests/` has end-to-end examples with real folders).
5. Describe what changes for the person using Charon, before and after.

## Safety first

Charon moves people's files. A change to the sync engine must never make it delete or overwrite
something it couldn't before without the person seeing it in the preview. When in doubt, keep
the file.

By contributing, you agree that your contributions are released under the [MIT licence](LICENSE).
