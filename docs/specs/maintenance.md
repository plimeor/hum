# Sync and checks
Type: feature

## Purpose

`hum apply` brings launchd in line with the config directory, and `hum doctor` checks for common problems. After a user adds, removes, or edits files in the config directory directly, `apply` makes the change take effect. Owned by the app module.

## Requirements

### Requirement: apply aligns launchd
`hum apply` MUST take the `services/` directory as the source of truth: generate and register a plist for each new config file, leaving a service that `hum stop` stopped not running; unregister and delete every hum plist that has no config file; regenerate and reload each outdated plist, with a service that was running before the reload still running after it. It MUST also keep the log rotation job in line: installed, current, and loaded while any service config exists, and unregistered and deleted once none does. `hum apply` MUST list every change it makes, the log rotation job's included, with the reason for each, and MUST say that launchd is in sync only when it changed nothing.

### Requirement: apply skips invalid configs
`hum apply` MUST skip invalid config files, report each one, carry on with the remaining services, and finally exit with a non-zero status.

### Requirement: apply dry run
`hum apply --dry-run` MUST only list the changes it would make, and MUST NOT change any file or launchd state.

### Requirement: doctor checks
`hum doctor` MUST check that the global config is valid and sets `PATH`; that each service config is valid; that `command` is found in the global `PATH`; that `cwd` exists; that every `env_file` exists; that the hum path in each plist exists and is executable, warning about a versioned path that breaks on upgrade; that the config and launchd agree; that no hum plist lacks a config; that the log rotation job is installed; and that no log has grown abnormally large.

### Requirement: doctor exit status
`hum doctor` MUST exit with a non-zero status when it finds an error-level problem, and with zero when it finds only warnings.
