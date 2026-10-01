# Scope

tauri-kit holds the runtime building blocks a Tauri desktop app ends up writing for itself — the
parts that ship inside the app and run on the user's computer. This document draws the line around
that job: what tauri-kit owns, what it deliberately leaves to its callers, and how to tell which
side a proposed capability falls on.

It exists because tauri-kit is a **shared core**. More than one app builds on it, and those apps do
not know about each other. Without a written boundary, each app's needs would accrete here one
reasonable-looking commit at a time, until the crates served every caller badly.

---

## The layer tauri-kit owns

**Platform behaviour a desktop app needs at run time and gets wrong when it writes it alone** —
the failures are quiet, platform-specific, and found late:

- **Files** — writes that survive a crash, renames that never replace, the brief refusals of virus
  scanners and sync clients, the conflict copies sync clients leave
- **Folders changing underneath** — changes made by other programs, reported once and without the
  app's own writes
- **Helper processes** — no console window, no orphans, a readiness wait that notices a crash, and
  the loopback arrangement an HTTP helper needs (with its .NET host side in `dotnet/`)
- **Secrets** — the operating system's credential store, with test and development builds kept off
  the installed app's entries
- **Per-folder state** — what the app keeps for each folder a person opens, outside that folder:
  a stable name for it, a format that older releases leave alone, writes kept until they land
- **Error reports** — what failed and where in the app's own code, never what it was about, kept
  where the person can read it before anything is sent

One capability per crate, so an app takes only what it needs. All crates share one version.

If an installed app is worse *as an app on that platform* without it, and it can be written
without knowing which app it serves, it probably belongs here.

## What tauri-kit does not do

Each of these is a deliberate non-goal, not a gap awaiting a contribution:

- **Development and verification tools.** Driving the window in end-to-end runs, checking
  installers, checking public text — anything that does not ship inside the app — belongs in
  [tauri-kit-dev](https://github.com/iyulab/tauri-kit-dev).
- **Know its callers.** No code path, option, default, comment, test fixture or commit message
  assumes a particular consuming app.
- **Hold a caller's constants.** File extensions, folder names, environment variable names,
  readiness-line prefixes, bundle identifiers, wording shown to people — all arrive as arguments.
  The crates supply no user-facing text.
- **Model a domain.** What an app's files mean — notes, records, documents, their formats and how
  they merge — is the app's. A crate knows bytes, paths and processes.
- **Interface components.** Buttons, layouts and design tokens belong to the UI packages.

## Dependency direction

**Upstream does not know downstream.** tauri-kit must not reference any consuming app by name.
When a comment needs to say why an option exists, describe the *situation* it serves ("an app whose
folder is synced by another program"), never the app.

---

## Is this request in scope?

Work through these in order. The first one that answers settles it.

1. **Does it ship inside the app and run on the user's computer?** If not, it is a tool, not a
   building block.
2. **Is it platform behaviour, rather than the app's own logic?** The test: would it be written
   the same way for an unrelated app on the same platform?
3. **Can it be stated without naming a caller?** Every constant it needs becomes a parameter; if
   the parameters are the whole capability, it is the caller's.
4. **Would a second, unrelated app want it?** A capability already written separately by two apps
   answers this; a hypothetical second app does not.

When the answer is unclear, prefer the narrower reading: a capability withheld can be added once a
second app shows the same need, while a capability released becomes a contract.
