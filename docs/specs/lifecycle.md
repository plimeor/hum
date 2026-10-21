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
A service with `autostart = true` that has not been stopped MUST start when the user logs in; a service with `autostart = false` MUST NOT start at login.

### Requirement: start
`hum start` MUST clear the disabled state left by `stop`, regenerate the plist from the current config, and start the service, including a service with `autostart = false`; when the service is already running it MUST do nothing.

### Requirement: stop
`hum stop` MUST stop the service and keep it from starting at later logins, until `hum start` or `hum restart` runs.

### Requirement: restart
`hum restart` MUST regenerate the plist from the current config and restart the service, so that changes to the config and `.env` files take effect then; a stopped service MUST be enabled again and started.

### Requirement: rm
`hum rm` MUST stop the service, delete its plist, and then delete its config file; unless `-y` is given it MUST ask for confirmation first; with `--keep-config` it only unregisters and keeps the config file. A service created later with the same name MUST NOT inherit the removed service's stopped state.

### Requirement: hum path in the plist
When a stable path to the current binary exists on `PATH` (such as a package manager's symlink), the hum path written into the plist MUST be that path rather than a versioned path that breaks on upgrade.
