# Running a service
Type: feature

## Purpose

`hum run <name>` reads the config, assembles the environment, changes directory, and replaces itself with the service command. launchd and manual foreground debugging take the same path. Owned by the run and environment assembly modules.

## Requirements

### Requirement: Replace itself with the service
`hum run <name>` MUST replace itself with the service command via exec in `cwd`, and MUST NOT stay in the process chain as a parent; the service command's exit status is the process's exit status.

### Requirement: launchd runs the same entry point
A launchd job hum generates MUST run `hum run <name>`, not the service command itself, so that changes to `command`, `cwd`, and environment variables take effect on the next start without re-registering.

### Requirement: Environment built from scratch
`hum run` MUST NOT inherit the caller's environment; it assembles only these layers, lowest priority first: the base variables `HOME`, `USER`, `SHELL`, `TMPDIR` (taken from the current process); the global `[env]`; the service's `env_file` entries in the order listed; the service's `[env]`.

#### Scenario: The caller's variables do not leak in
- **WHEN** a user runs `hum run t3` from a shell with `LANG=zh_CN.UTF-8`, and neither the global nor the service config sets `LANG`
- **THEN** the service process's environment has no `LANG`

### Requirement: Command lookup uses the assembled PATH
When the first argument of the command contains no `/`, it MUST be looked up in the `PATH` of the assembled environment; when it is not found, `hum run` MUST exit with a non-zero status without starting the service.

### Requirement: Refuse to start without an env file
When any `env_file` is missing or unreadable, `hum run` MUST exit with a non-zero status, MUST NOT start the service, and MUST write the missing path to standard error (the service's log under launchd).

### Requirement: Check the log size before starting
Before starting the service, `hum run` MUST check the service's log and rotate it first when it has reached `max_size`.
