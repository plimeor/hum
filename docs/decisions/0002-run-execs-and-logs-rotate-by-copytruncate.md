# 0002: `hum run` execs the service; logs rotate by copy and truncate

Status: Accepted
Date: 2026-10-21

## Context

launchd starts every service as `hum run <name>`. hum has to load the config and build the environment before the service starts, and something has to own the service process and its output. launchd opens `StandardOutPath` / `StandardErrorPath` when it starts a job and hands the open descriptors to the process it starts. launchd does not rotate logs.

## Decision

`hum run` loads the config, builds the environment, changes directory, and then replaces itself with the service via exec. The process launchd supervises is the service, and the service writes straight to the log descriptors launchd opened.

Logs rotate by copy and truncate: the log is copied to a timestamped file, then truncated in place. This works only because launchd opens the log with `O_APPEND`, so writes after the truncate land at the new end of the file. That was checked on a Mac with a test job: both descriptors carried the append flag, a truncate from 4,104 to 144 bytes left no NUL bytes, and a restarted job appended to the same file.

## Considered options

- **`hum run` stays as the parent and writes the logs itself (the PM2 model).** Rotation could reopen files cleanly, lose no lines, and prefix timestamps. Rejected: every service would carry a resident hum process; hum would have to forward signals, pass on exit codes, and reap children, which makes it the daemon hum is not meant to have; launchd would supervise hum instead of the service; and a crash in hum would take the service's output with it.
- **Rotate by renaming the log.** Rejected: the service keeps its descriptor and goes on writing to the renamed file.

## Consequences

- launchd delivers signals to the service directly; the service's exit status drives `restart = "on-failure"`; `hum ls` shows the service's real PID.
- Lines written between the copy and the truncate are lost.
- hum never sees log lines, so it cannot add timestamps or filter output.
- Rotation depends on launchd keeping `O_APPEND`. If a macOS release changes that, truncation leaves the service writing at its old offset and the log fills with a NUL-byte hole; the fallback is the parent-process option above.
