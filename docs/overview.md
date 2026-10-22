# Project overview
Updated 2026-10-22

## What this is

hum is a macOS CLI written in Rust that registers long-running background commands (such as `t3 server`) as launchd services and manages them through TOML files in one directory.

## Principles

- **One file is one service**: like fish's `functions/`, each `<name>.toml` under `services/` is one service, named after its file. The file is the single source of truth; the plist is generated from it.
- **launchd does the supervising**: keeping services alive, restarting them after a crash, and starting them at login are the system's job; hum has no daemon of its own.
- **Manual and managed runs behave the same**: launchd starts `hum run <name>`, and foreground debugging uses the same command, so nothing works by hand yet fails under launchd.
- **Config changes need no re-registration**: the command, working directory, and environment variables are read at start time, so a restart is enough.

## Status
Shipped features are described in `docs/specs/`.

## Current focus

v1: macOS only (arm64 / x64), all 14 commands. The code and tests are done, and releases ship through Homebrew (`plimeor/tap`). On a real Mac with 0.1.0-rc.4, these checks pass:

- The real-launchd tests: `cargo test -- --ignored --test-threads=1 launchd::macos`.
- Local integration: after `hum new sleeper -- sleep 1000`, `ls` shows running; after `kill -9` it is started again within 10 seconds; after `stop` it is not started again; after `rm` both the plist and the config are gone.
- Environment: a service run by launchd and the same service run with `hum run` in the foreground see the same environment.
- Upgrades: after upgrading hum with brew, services still restart normally.
- launchd opens logs with `O_APPEND`, so copytruncate rotation works.

What remains:

- Login and reboot: after logging out and back in, and after a reboot, started services come back and stopped ones do not.
- `t3 serve` has run as the first real service (`~/.config/hum/services/t3.toml`, registered 2026-10-22) for a full week.

## Next

Release v0.1.0 once both remaining checks pass.

## Non-goals

- Linux / systemd support
- Health checks, dependencies between services, scheduled jobs, resource limits
- A TUI dashboard
- System-wide daemons running as root (`/Library/LaunchDaemons`)
