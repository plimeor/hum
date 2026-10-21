# Service status
Type: feature

## Purpose

`hum ls` lists every service and its state; `hum show` shows one service's resolved config, the environment variables in effect, and its launchd state. Owned by the app and CLI modules.

## Requirements

### Requirement: Service state
Each service MUST be shown in exactly one state: `running`; `disabled` (stopped by `hum stop`); `crashed` (not running, and its last exit was non-zero or by a signal); `stopped` (any other case of not running).

### Requirement: ls output
`hum ls` MUST print one row for every service that has a config file or a hum plist, with the columns NAME, STATE, PID, LAST EXIT, COMMAND; a service whose config file is invalid MUST still be listed, with the error.

### Requirement: Flag mismatches with launchd
When what runs does not match the config, `hum ls` MUST mark the service: the plist is missing or outdated, a plist has no config, the job was loaded from another path, or a running service started before its service config, the global config, or one of its env files last changed. Pointing a symbolic link on such a file's path at another file counts as a change to that file. `hum show` MUST say which of these applies.

### Requirement: show output
`hum show <name>` MUST print the full merged config (including the log rotation settings in effect), the launchd state, and every environment variable in effect together with the layer that set it (base variables, global `[env]`, which env file, or the service's `[env]`).

### Requirement: Environment values masked by default
`hum show` MUST mask every environment variable value by default and show values only with `--reveal`.

### Requirement: show reports environment problems
`hum show` MUST report undefined variable references in env files (file and line), and say that `hum run` would refuse to start when an env file is missing; these problems MUST NOT stop the rest of the output.

### Requirement: JSON output
With `--json`, `hum ls` and `hum show` MUST print the same content as JSON, with the same masking in `show`.
