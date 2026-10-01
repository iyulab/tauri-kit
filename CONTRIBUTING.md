# Contributing

## Before proposing a capability: read the scope

tauri-kit is a **shared core** — more than one app builds on it, and those apps do not know about
each other. **[SCOPE.md](SCOPE.md) is the gate**: it states what belongs here, what
deliberately does not, and a numbered test for deciding which side a request falls on. Include
that reasoning in your proposal.

Two rules come up often enough to repeat:

- **No consuming app is named** — in code, comments, documentation, test fixtures or commit
  messages. Describe the situation a capability serves, never the app that has it.
- **Constants are parameters.** An app name, a folder name, a file extension, an environment
  variable prefix: the caller passes it.

## Checking public text

CI runs the public-text check from [tauri-kit-dev](https://github.com/iyulab/tauri-kit-dev) with
[scripts/public-text.config.js](scripts/public-text.config.js): local paths, private hosts, and the
traces a private workspace tends to leave behind, in tracked files and in commit messages not yet
on `main`. It names no one, so it cannot check for *your* app's names. Before pushing, run it with
your own list as well, kept outside this repository:

```sh
npx @iyulab/tauri-kit-dev public-text --config scripts/public-text.config.js .
npx @iyulab/tauri-kit-dev public-text --config <your private config> .
```

## Development

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-features
```

All three run in CI on Linux, Windows and macOS, and the workspace is also checked with the
declared minimum compiler (`rust-version`).

## Conventions

- **Language** — everything in this repository is written in English.
- **Tests** — behaviour changes come with tests.
- **Changelog** — user-visible changes get an entry in [CHANGELOG.md](CHANGELOG.md) under
  `## [Unreleased]`, in the same commit. Other apps learn what changed from it.
- **Versioning** — the crates share one version. While it is `0.x`, a minor bump may break the
  API; a patch bump does not.
