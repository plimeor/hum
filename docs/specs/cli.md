# Command-line conventions
Type: feature

## Purpose

The arguments, multi-service handling, JSON output, and exit status shared by every hum command. Owned by the CLI module.

## Requirements

### Requirement: Multiple names and --all
`start`, `stop`, `restart`, `rm`, and `logs` MUST accept several service names or `--all` (every service with a config file), but not both. When one service fails in `start`, `stop`, `restart`, `rm`, or `logs --rotate`, hum MUST report it, carry on with the rest, and finally exit with a non-zero status.

### Requirement: Skip confirmation
The global `-y` flag MUST skip every confirmation prompt and count as agreeing.

### Requirement: Completion scripts
`hum completion fish|zsh|bash` MUST print the completion script for that shell, and completion MUST offer the configured service names.

### Requirement: Home directory is a real path
When `$HOME` is a symbolic link or lies under one, every command that reads the config directory MUST fail and name the real path `$HOME` resolves to.

### Requirement: Version
`hum --version` MUST print hum's version.

### Requirement: Exit status on failure
When any command fails, it MUST print the reason to standard error and exit with a non-zero status.

### Requirement: JSON for every command
With the global `--json` flag, every command except `edit`, `run`, `completion`, and `logs --follow` MUST print exactly one JSON document on standard output and nothing else, and MUST exit with the same status as without the flag. When a command fails before it has a result, the document MUST be an object whose `error` field holds the reason. `edit`, `run`, `completion`, and `logs --follow` MUST refuse `--json`. Warnings and errors still go to standard error.

### Requirement: JSON results per service
With `--json`, `start`, `stop`, `restart`, `rm`, and `logs --rotate` MUST print an array with one object per service, and `new`, `set`, and `unset` one such object. Each object MUST carry the service `name`, an `outcome`, and its `warnings`; a service that failed MUST have the outcome `failed` and an `error`. The outcome is one of `created`, `started`, `already_running`, `restarted`, `needs_restart`, `applied`, `unchanged`, `stopped`, `removed`, `unregistered`, `rotated`, `not_rotated`, `failed`. A service launchd was asked to run but `hum run` refuses to start MUST carry `not_running` with the `reason` and whether launchd keeps `retrying`.

### Requirement: JSON for the other commands
With `--json`, `hum logs` MUST print an array of objects with each service's `name` and its `lines`; `hum apply` an object with `dry_run`, the `actions` (each with `kind`, `subject`, `reason`, and an `error` on the one that failed), and the `skipped` configs; `hum doctor` an array of findings, each with `severity` (`ok`, `warn`, `error`), `subject`, and `message`.
