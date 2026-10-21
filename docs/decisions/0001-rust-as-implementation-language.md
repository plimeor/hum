# 0001: Implement hum in Rust

Status: Accepted
Date: 2026-10-21

## Context

hum was first implemented in Go, with all 14 commands. The user's view was that less code usually means fewer bugs, so the question was how much code a different language would save. Two facts framed the comparison:

- Two pieces of logic have to be hand-written whatever the language: `hum set` / `unset` keep comments when changing a config, and `.env` expansion must resolve against lower-priority layers (base variables, global `[env]`, earlier env files) and report undefined references.
- `hum run` replaces itself with the service process via exec; see `docs/specs/run.md`.

## Decision

hum is implemented in Rust, replacing the Go version.

## Considered options

- **Go (the original)**: cobra gives subcommands, help, and completion. Most of the extra code is explicit error handling, about 340 error checks in production code. The comment-preserving TOML editor is hand-written, 416 lines.
- **Bun / TypeScript**: `Bun.TOML.stringify` writes from a plain object and drops comments, and `util.parseEnv` does not expand variables, so neither hand-written piece goes away. `util.parseArgs` has no subcommands or completion. `bun build --compile` produces a 77.5 MB hello world. `process.execve` works in Bun 1.4.2 on Linux; macOS was not checked. Rejected because the savings are mostly error-handling boilerplate, the hard parts stay, and a single-file binary is an order of magnitude larger.
- **Rust (chosen)**:
  - `toml_edit` preserves comments, shrinking the hand-written editor to about 60 lines;
  - serde's `deny_unknown_fields` and enums express strict parsing directly;
  - clap and clap_complete provide subcommands and dynamic completion;
  - the standard library has exec;
  - `dotenvy` resolves references only against the process environment and does not report undefined ones, so `.env` parsing stays hand-written.

## Consequences

- Production code (blank lines and comments excluded, each formatted with its default tool) went from 3,508 lines of Go to 2,769 lines of Rust, about 21% fewer. The release binary went from 5.1 MB to 2.9 MB.
- Most of the saving comes from error handling and the TOML editor; layered env expansion and similar logic are language-independent and still have to be right.
- Dynamic completion depends on `clap_complete`'s `unstable-dynamic` feature; check that completion still works when upgrading it.
- The Rust version needs its own validation on a Mac before release; the Go version's results do not carry over.
