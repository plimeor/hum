# Global config file
Type: format

## Purpose

`config.toml` in the config directory holds the environment variables every service shares and the log rotation defaults. It is optional: hum writes it only when the user saves it with `hum edit --global`, and each service records its own `PATH` (see Creating and changing configs). Owned by the config module.

## Fields
| Field | Type | Default | When invalid |
|---|---|---|---|
| `[env]` | table of string to string; values are taken literally | `{}` | The file is invalid when a value is not a string or a key does not match `[A-Za-z_][A-Za-z0-9_]*` |
| `[log].max_size` | size string, same format as in the service config | `"10MB"` | The file is invalid when it cannot be parsed or is not positive |
| `[log].keep` | non-negative integer | `3` | The file is invalid when negative |

## Requirements

### Requirement: Strict parsing
hum MUST reject a global config with an unknown field, a type mismatch, or an illegal value; a missing file MUST be treated as every field at its default.

### Requirement: Service log settings override global ones
`[log].max_size` and `[log].keep` in a service config MUST each override the global value, and an item the service does not set MUST take the global value.
