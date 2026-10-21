# 0003: Starting at login follows `hum start` / `hum stop`

Status: Accepted
Date: 2026-10-21

## Context

Service configs had an `autostart` field, rendered as the plist's `RunAtLoad`. Validation on a Mac showed that a service with `autostart = false` and the default `restart = "always"` started anyway when its plist was loaded: launchd starts a `KeepAlive = true` job as soon as it loads it. launchd.plist(5) states that `KeepAlive = { SuccessfulExit = false }`, used for `on-failure`, implies `RunAtLoad` as well (not tested on the Mac), so only `restart = "never"` could honour `autostart = false`.

A running service needs a loaded launchd job whatever its login behaviour, because launchd is what restarts it, routes its output to the log, and reports its state. And what makes launchd load a job at login is where its plist lives, `~/Library/LaunchAgents`, together with the job's enabled or disabled state, which `hum stop` and `hum start` already set and launchd keeps across logins.

## Decision

The `autostart` field is removed. Every plist sets `RunAtLoad`, and whether a service starts at login follows the last `hum start` or `hum stop`: a started service comes back at login, a stopped one stays stopped. The service config describes what a service is and what happens when it exits; whether it should be running is state that the commands own.

## Considered options

- **Keep `autostart`, and store the plists of `autostart = false` services outside `~/Library/LaunchAgents`, loading them only on `hum start`.** It would work, but it gives hum two plist locations to track, move between, and check, for a setting that duplicates what `start` and `stop` already persist. Rejected.
- **Keep `autostart`, and `launchctl disable` such a service right after starting it.** Disabled would then mean both "stopped" and "do not start at login", so `hum ls` could not tell them apart, and whether launchd keeps restarting a disabled job that is still loaded was not verified. Rejected.
- **Allow `autostart = false` only with `restart = "never"`.** It keeps a field that is meaningful for one restart policy out of three. Rejected.

## Consequences

- A config that still sets `autostart` is rejected as having an unknown field.
- "Running now, but not after the next login" is no longer expressible; `hum stop` the service when it is no longer needed.
- `hum new --no-autostart` and the `autostart` key of `hum set` / `unset` are gone.
