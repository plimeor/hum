# Logs
Type: feature

## Purpose

Each service's output goes to one log file. `hum logs` views, follows, or rotates it, and hum rotates logs that grow past their size limit on its own. Owned by the logfile module.

## Requirements

### Requirement: Log location
Each service's standard output and standard error MUST be written together to `~/Library/Logs/hum/<name>.log`.

### Requirement: View logs
`hum logs <name>...` MUST show the last 100 lines of each log by default, `-n` setting the count; when viewing several services or with `--all`, every line MUST carry a `[name]` prefix.

### Requirement: Follow logs
`hum logs -f` MUST keep printing newly written lines until interrupted; after a log is truncated by rotation it MUST continue with the new content.

### Requirement: Automatic rotation
Once a log reaches its effective `max_size`, hum MUST rotate it without any user action; how often this is checked is unspecified.

### Requirement: How rotation works
Rotation MUST copy the current log to `<name>__YYYY-MM-DD_HH-mm-ss.log` in the same directory (local time, with a `_1`, `_2` suffix when rotated more than once in the same second), then truncate the original to empty; it MUST NOT rename or replace the original. Only the newest `keep` copies are kept and older ones MUST be deleted. Lines written between the copy and the truncate may be lost.

#### Scenario: A service writing its log is unaffected
- **WHEN** service `t3` is running and its log is rotated
- **THEN** `t3`'s later output keeps going to `~/Library/Logs/hum/t3.log` without a restart

### Requirement: Empty logs are not rotated
An empty log MUST NOT be rotated and MUST NOT produce a copy.

### Requirement: Rotate now
`hum logs <name>... --rotate` MUST rotate the given logs right away regardless of size; for an empty log it MUST say there is nothing to rotate.
