# Env file
Type: format

## Purpose

The `.env` files a service's `env_file` points to: one variable assignment per line, able to reference variables defined before it. Owned by the dotenv module.

## Fields
| Field | Type | Default | When invalid |
|---|---|---|---|
| Assignment line | `KEY=VALUE`, optionally prefixed with `export `; `KEY` starts with a letter or `_` and holds letters, digits, `_`, `.` | — | The whole file is invalid on a missing `=`, an illegal name, or an unclosed quote |
| Comment | A line starting with `#`; in an unquoted value, a `#` preceded by whitespace starts an inline comment | — | — |
| Unquoted value | Runs to the end of the line, surrounding whitespace removed; variables expand; `\$` gives a literal `$` | `""` | — |
| Single-quoted value `'…'` | Literal, never expanded, may span lines | — | The file is invalid when unclosed |
| Double-quoted value `"…"` | Escapes `\n` `\r` `\t` `\"` `\\` `\$`; variables expand; may span lines | — | The file is invalid when unclosed |

## Requirements

### Requirement: Variable expansion
`${VAR}` and `$VAR` in unquoted and double-quoted values MUST expand against the variables assembled so far: the lower-priority layers (base variables, global `[env]`, earlier env files) and the lines above in the same file.

### Requirement: Forward-only references
A reference MUST resolve only to variables defined before its line; it cannot see variables defined later or the service's own `[env]`. An undefined variable MUST expand to the empty string and MUST be reportable by `hum show`.

#### Scenario: Referencing a variable defined later
- **WHEN** line 1 of `.env` is `URL=http://localhost:${PORT}`, line 2 is `PORT=8080`, and no lower layer defines `PORT`
- **THEN** `URL` is `http://localhost:`, and `hum show` reports that line 1 references the undefined `PORT`

### Requirement: A `$` that is not a reference stays literal
A `$` followed by something other than a valid variable name, or by an unclosed `${`, MUST stay as literal text.
