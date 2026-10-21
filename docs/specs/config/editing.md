# Creating and changing configs
Type: feature

## Purpose

Users create and change service configs with `hum new`, `hum edit`, `hum set`, and `hum unset`; a change is written only after it validates, and takes effect by the rules below. Owned by the CLI module.

## Requirements

### Requirement: Create a service
`hum new <name> [flags] -- <cmd> [args...]` MUST write the arguments after `--` unchanged as the `command` array, default `cwd` to the current directory, and accept `--cwd`, repeatable `--env K=V` and `--env-file PATH`, and `--restart`; unless `--no-start` is given, it MUST start the service right away.

### Requirement: New never overwrites
When a service of the same name exists, `hum new` MUST fail and MUST NOT change the existing config.

### Requirement: Warnings do not block writes
When the first argument of `command` is not found in the global `PATH`, `hum new` MUST warn and still write the config. When an `env_file` does not exist, `hum new`, `edit`, `set`, and `unset` MUST warn and still write the config.

### Requirement: Edit in an editor
`hum edit <name>` and `hum edit --global` MUST open a temporary copy in `$VISUAL`, falling back to `$EDITOR` and then `vi`; after saving it validates the copy. On failure it MUST show the error and ask whether to edit again, leaving the original file unchanged; a closed stdin counts as no. On success it MUST replace the original file atomically.

### Requirement: Change fields from the command line
`hum set <name> <key>=<value>...` and `hum unset <name> <key>...` MUST accept only `command`, `cwd`, `env_file`, `restart`, `env.<NAME>`, `log.max_size`, and `log.keep`. An `env_file` value separates several paths with commas. `command` MUST be set through `hum set <name> command -- <cmd> [args...]` and cannot be unset.

### Requirement: Edits keep comments
`hum set` and `hum unset` MUST change only the content of the target key; every other piece of text in the file, comments included, MUST stay unchanged.

### Requirement: Invalid results are not written
When the result of `hum set` or `hum unset` is not a valid config, the command MUST fail and MUST NOT change the file.

### Requirement: When a service config change takes effect
After `edit`, `set`, or `unset` changes a service config, hum MUST restart the service automatically if it is running; with `--no-restart` it MUST NOT restart it and MUST say that a restart is needed. When launchd has the job loaded but not running and the change alters what launchd holds (`restart`), hum MUST NOT start the service and MUST say that a restart is needed; any other change takes effect when the service next starts. Until a waiting change takes effect, `hum ls` MUST flag the service.

### Requirement: Global config changes restart nothing
After `hum edit --global` changes the global config, hum MUST NOT restart any service and MUST print one line saying the change takes effect when each service next restarts.
