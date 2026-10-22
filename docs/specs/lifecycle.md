# Service lifecycle
Type: feature

## Purpose

`hum start`, `stop`, `restart`, and `rm` control through launchd whether a service runs, starts at login, and restarts after it exits. Owned by the app module, with launchd calls behind the launchd module.

## Requirements

### Requirement: launchd job location and label
Each service MUST be registered as a LaunchAgent of the current user, with label `hum.<name>` and plist at `~/Library/LaunchAgents/hum.<name>.plist`; only hum generates that plist.

### Requirement: Restart policy
A service with `restart = "always"` MUST be started again by launchd every time it exits; `"on-failure"` MUST be started again only after a non-zero exit; `"never"` MUST NOT be started again. launchd decides the interval between starts.

### Requirement: Start at login
Whether a service starts when the user logs in MUST follow the last `hum start` or `hum stop`: a service that was started and not stopped since MUST start at login, and a stopped service MUST NOT.

### Requirement: start
`hum start` MUST clear the disabled state left by `stop`, regenerate the plist from the current config, and start the service; when the service is already running it MUST do nothing.

### Requirement: Report a service that cannot start
When `hum new`, `start`, or `restart`, or the automatic restart after a config change, asks launchd to run a service that `hum run` would refuse to start, hum MUST still register it and MUST say that it is not running and why, instead of reporting it started.

### Requirement: stop
`hum stop` MUST stop the service and keep it from starting at later logins, until `hum start` or `hum restart` runs.

### Requirement: restart
`hum restart` MUST regenerate the plist from the current config and restart the service, so that changes to the config and `.env` files take effect then; a stopped service MUST be enabled again and started.

### Requirement: rm
`hum rm` MUST stop the service, delete its plist, and then delete its config file and its log together with every rotated copy; unless `-y` is given it MUST ask for confirmation first; with `--keep-config` it only unregisters and keeps the config file and logs. When the last service is removed, the log rotation job's own log MUST be deleted with the job. A service created later with the same name MUST NOT inherit the removed service's stopped state.

### Requirement: launchd records stay bounded
launchd keeps a permanent per-user record of every job that was ever enabled or disabled, which only root can delete. hum MUST add such a record only when `hum stop` disables a service; `start`, `restart`, `rm`, `apply`, and installing the log rotation job MUST NOT add one for a job that is not disabled.

### Requirement: Launcher link
Each job's plist MUST run a symbolic link at `~/Library/Application Support/hum/bin/hum (<name>)`, or `hum (log rotation)` for the log rotation job, instead of the hum binary, so macOS lists the job under that name in Login Items. The link MUST point at the hum path, and MUST be deleted together with the job's plist.

### Requirement: hum path behind the launcher
When a stable path to the current binary exists on `PATH` (such as a package manager's symlink), the launcher link MUST point at that path rather than a versioned path that breaks on upgrade. A hum started through a launcher link MUST take the link's target as its own path.
