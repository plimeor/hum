# hum

Read `docs/overview.md` before starting work.

## Code map

| Concept | Location |
|---|---|
| Commands and flags | `internal/cli` |
| Operations (start, stop, apply, doctor, run preparation) | `internal/app` |
| Config files, paths, validation | `internal/config` |
| `.env` parsing and expansion | `internal/dotenv` |
| Environment assembly for `hum run` | `internal/envx` |
| launchctl wrapper (`Manager` interface) | `internal/launchd` |
| plist rendering | `internal/plist` |
| Comment-preserving TOML edits (`set` / `unset`) | `internal/tomledit` |
| Log rotation, tail, follow | `internal/logfile` |

Checks: `go vet ./...` and `go test ./...` run on any OS; launchd behavior needs a real Mac.

## Docs conventions
- `docs/overview.md`: what this project is, its principles, current focus, and non-goals. Read it before starting work. Non-goals are not to be implemented or proposed.
- `docs/specs/`: the promises each shipped feature makes to its users, one file per feature, grouped in area subdirectories where a feature area holds several specs; what a spec leaves unspecified is the implementation's choice and may change without notice. Update the spec in the same change as the code. When a spec and the code disagree, the code is right and the spec gets fixed, unless the code breaks a guarantee users rely on; that is reported as a suspected bug for the user to decide.
- `docs/plans/`: intent for features being built or queued. Everything under it, including the design (`design.md`, `design/`), attachments, and the spec change files in `docs/plans/<slug>/specs/`, is a proposal and not a description of current behavior. When implementing a plan, read the Tasks in its `plan.md` to find what is open, follow its design, and tick each task as its work lands.
- `docs/archive/`: shipped plans kept as history: `plan.md`, the design, and attachments. Nothing in it describes current behavior or is work to do; the spec changes each plan made are merged into `docs/specs/` and recorded in its git history. Read it only to learn why something was built the way it was. Its contents are not edited.
- `docs/decisions/`: cross-feature technical decisions. Records directly in it are live: `Status: Accepted` is in force; an approach recorded as `Rejected`, or turned down in a live record's Considered options, is not to be proposed again unless the user reopens it. `docs/decisions/superseded/` is history; read it only to learn why a decision changed. A record on the default branch is changed by a new record that supersedes it; a record not yet there is edited in place.
