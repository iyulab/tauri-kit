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

## Cutting a release

One commit, `chore: release <version>`, moves every version line together:

1. `version` in the root `Cargo.toml`, and every requirement one crate here has on another
   (`grep -rn 'path = "\.\./' crates/*/Cargo.toml` — today `tauri-kit-fs` in `crates/diagnostics`, `crates/state`,
   `crates/update` and `crates/watch`, and `tauri-kit-state` in `crates/diagnostics`).
2. `<Version>` in `dotnet/Directory.Build.props`.
3. The README: the tag in the git-dependency example and, on a minor bump, the `"0.x"` requirements in the
   usage snippet.
4. `Cargo.lock` (`cargo update -w`) and a `## [<version>] - <date>` heading under `## [Unreleased]` in the
   CHANGELOG, with its compare link at the bottom.

Then tag `v<version>`, push the branch and the tag, run `gh workflow run publish-nuget.yml --ref v<version>`,
and `cargo publish -p <crate>` for each crate, `tauri-kit-fs` first.
`cargo publish` packages the working tree, not the tag: publish from a checkout of the tag (`git checkout
v<version>`), with `git status` clean and `git rev-parse HEAD` equal to `git rev-parse v<version>^{commit}`.
