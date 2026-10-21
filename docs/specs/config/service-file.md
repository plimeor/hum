# Service config file
Type: format

## Purpose

`services/<name>.toml` describes one service: what it runs, where, with which environment, whether it starts at login, and whether it is restarted after it exits. It is the service's single source of truth; the launchd plist is generated from it. Owned by the config module.

## Fields
| Field | Type | Default | When invalid |
|---|---|---|---|
| `command` | array of strings (argv), or a string (run through `/bin/sh -c`) | required | The whole file is invalid when it is missing, an empty array, an empty string, or holds a non-string element |
| `cwd` | path string; `~` and `$HOME` are expanded | empty, meaning the user's home directory | The file is invalid on a type mismatch; a directory that does not exist does not affect parsing and fails at `hum run` |
| `env_file` | array of path strings, relative to `cwd`; `~` and `$HOME` are expanded | `[]` | The file is invalid when an entry is an empty string |
| `autostart` | boolean | `true` | The file is invalid on a type mismatch |
| `restart` | `always` \| `on-failure` \| `never` | `always` | The file is invalid on any other value |
| `[env]` | table of string to string; values are taken literally | `{}` | The file is invalid when a value is not a string or a key does not match `[A-Za-z_][A-Za-z0-9_]*` |
| `[log].max_size` | size string such as `"10MB"`; units `B`/`K`/`KB`/`M`/`MB`/`G`/`GB`, powers of 1024 | inherited from the global config | The file is invalid when it cannot be parsed or is not positive |
| `[log].keep` | non-negative integer | inherited from the global config | The file is invalid when negative |

## Requirements

### Requirement: File name is the service name
A service config MUST live at `services/<name>.toml` in the config directory; the service name is taken from the file name and MUST match `[a-z0-9][a-z0-9_-]*`.

### Requirement: Config directory location
The config directory MUST default to `~/.config/hum`; when `$HUM_HOME` is set it MUST be used instead, and `hum run` started by launchd MUST read the same directory.

### Requirement: Strict parsing
hum MUST reject a config file with an unknown field, a type mismatch, or an illegal value, and the error MUST name the field or position at fault; an invalid file MUST NOT be partly applied.

### Requirement: Command as a string
When `command` is a string, hum MUST run it as `/bin/sh -c <command>`; when it is an array, hum MUST run that argv directly, without a shell.

### Requirement: Path expansion
A leading `~` and `$HOME` / `${HOME}` in `cwd` and `env_file` MUST expand to the user's home directory, and a relative `env_file` MUST resolve against `cwd`; values in `[env]` MUST be taken literally, with no expansion.
