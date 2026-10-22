# Global config file
Type: format

## Purpose

`config.toml` in the config directory holds the environment variables every service shares and the log rotation defaults. Owned by the config module.

## Fields
| Field | Type | Default | When invalid |
|---|---|---|---|
| `[env]` | table of string to string; values are taken literally | `{}` | The file is invalid when a value is not a string or a key does not match `[A-Za-z_][A-Za-z0-9_]*` |
| `[log].max_size` | size string, same format as in the service config | `"10MB"` | The file is invalid when it cannot be parsed or is not positive |
| `[log].keep` | non-negative integer | `3` | The file is invalid when negative |

## Requirements

### Requirement: Strict parsing
hum MUST reject a global config with an unknown field, a type mismatch, or an illegal value; a missing file MUST be treated as every field at its default.

### Requirement: Created on first run
When `config.toml` does not exist, every hum command other than `hum run`, help, and completion MUST create it, writing the current shell's `PATH` into `[env]`.

#### Scenario: A launchd start does not record the short PATH
- **WHEN** the config directory has no `config.toml` and launchd runs `hum run t3`
- **THEN** hum does not create `config.toml`

### Requirement: Per-shell PATH entries are resolved
When hum creates `config.toml`, each `PATH` entry that lies in a directory a version manager creates for a single shell session (a path with an `fnm_multishells` component) MUST be written as the directory it resolves to, so services keep the version that shell used after the session ends; an entry that does not resolve and every other entry MUST be written unchanged.

#### Scenario: fnm's per-shell node directory
- **WHEN** the shell's `PATH` starts with `~/.local/state/fnm_multishells/81142_1785983258477/bin`, which resolves to `~/.local/share/fnm/node-versions/v24.21.0/installation/bin`
- **THEN** the `PATH` in the new `config.toml` starts with `~/.local/share/fnm/node-versions/v24.21.0/installation/bin`, written as an absolute path

### Requirement: Service log settings override global ones
`[log].max_size` and `[log].keep` in a service config MUST each override the global value, and an item the service does not set MUST take the global value.
