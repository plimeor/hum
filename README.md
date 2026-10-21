# hum

Run long-running commands as macOS launchd services, configured by TOML files in one directory.

Each `<name>.toml` under `services/` is one service, named after its file. launchd keeps services alive, restarts them after a crash, and starts them at login; hum has no daemon of its own. launchd starts `hum run <name>`, and you debug in the foreground with the same command, so a service behaves the same both ways. Change the command, working directory, or environment and `hum restart` picks it up; nothing needs re-registering.

macOS only (arm64 / x64). There is no release yet, so install from source; validation on a Mac is still in progress.

## What it looks like

```console
$ hum new t3 -- t3 server
created /Users/me/.config/hum/config.toml with the current shell's PATH
created /Users/me/.config/hum/services/t3.toml
t3: started
$ hum ls
NAME  STATE    PID   LAST EXIT  COMMAND
t3    running  4242  -          t3 server
$ hum logs t3 -f
```

On its first run hum creates the global config and records the current shell's `PATH` in it, because the `PATH` launchd gives services is short.

## Usage

### Service config

```toml
# ~/.config/hum/services/t3.toml
command = ["t3", "server"]   # argv; a string runs through /bin/sh -c
cwd = "~/work/t3"
env_file = [".env"]          # relative to cwd; later files override earlier ones
autostart = true             # start at login
restart = "always"           # always | on-failure | never

[env]
PORT = "3773"

[log]
max_size = "10MB"            # inherited from the global config when unset
keep = 3
```

| Field | Default | Notes |
|---|---|---|
| `command` | required | An array runs as argv; a string runs through `/bin/sh -c` |
| `cwd` | your home directory | `~` and `$HOME` are expanded |
| `env_file` | `[]` | `hum run` refuses to start when any of them is missing |
| `autostart` | `true` | Whether to start at login |
| `restart` | `always` | `on-failure` restarts only after a non-zero exit |
| `[env]` | `{}` | Values are taken literally, with no expansion |
| `[log]` | global | `max_size` uses powers of 1024; `keep` is how many old copies to keep |

Unknown fields and illegal values are errors, and an invalid file is never partly applied. The global config, `~/.config/hum/config.toml`, has only `[env]` and `[log]`, shared by every service. Set `$HUM_HOME` to keep the config directory somewhere else.

### Environment

`hum run` does not inherit your environment; it builds one from scratch. Lowest priority first:

1. `HOME`, `USER`, `SHELL`, `TMPDIR` from the current process
2. The global `[env]`
3. The service's `env_file` entries, in the order listed
4. The service's `[env]`

`${VAR}` and `$VAR` in `.env` files expand, but only to lower layers and lines above in the same file. An undefined one becomes the empty string, which `hum show` points out; single-quoted values never expand, and `\$` gives a literal `$`.

### Commands

| Command | What it does |
|---|---|
| `hum new <name> [flags] -- <cmd> [args...]` | Create a service and start it. Flags: `--cwd`, `--env K=V`, `--env-file PATH`, `--restart`, `--no-autostart`, `--no-start` |
| `hum edit <name>` / `hum edit --global` | Edit in `$VISUAL` / `$EDITOR` (`vi` when neither is set); saved only when valid |
| `hum set <name> <key>=<value>...` | Change fields, keeping comments in the file, e.g. `hum set t3 env.PORT=4000 restart=on-failure` |
| `hum set <name> command -- <cmd> [args...]` | Change `command` |
| `hum unset <name> <key>...` | Remove fields |
| `hum rm <name>...` | Stop the service and delete its plist and config; `--keep-config` only unregisters |
| `hum start <name>...` | Start, clearing a previous `stop` |
| `hum stop <name>...` | Stop, and stay stopped across logins until `start` |
| `hum restart <name>...` | Regenerate the plist from the current config and restart |
| `hum ls` | List services and their state (running / stopped / crashed / disabled) |
| `hum show <name>` | The merged config, where each environment variable comes from, and the launchd state; values are masked unless `--reveal` |
| `hum logs <name>...` | The last 100 lines; `-n` sets the count, `-f` follows, `--rotate` rotates now |
| `hum apply` | Bring launchd in line with `services/` after editing files there directly; `--dry-run` only lists the changes |
| `hum doctor` | Check for common problems; exits non-zero when it finds an error |
| `hum run <name>` | Run a service in the foreground; this is also what launchd runs |
| `hum completion fish\|zsh\|bash` | Print a completion script |

Commands that take `<name>...` accept several names, or `--all` for every service. The global `--json` flag makes `ls` and `show` print JSON, and `-y` skips confirmations.

After `edit`, `set`, or `unset` changes a service config, a running service restarts automatically; `--no-restart` turns that off. Changing the global config restarts nothing; run `hum restart --all` to apply it right away.

### Where files live

| What | Path |
|---|---|
| Service configs | `~/.config/hum/services/<name>.toml` |
| Global config | `~/.config/hum/config.toml` |
| launchd plists (generated by hum) | `~/Library/LaunchAgents/hum.<name>.plist` |
| Logs (stdout and stderr together) | `~/Library/Logs/hum/<name>.log` |

Every launchd label is `hum.<name>`, so `launchctl list | grep hum.` shows them.

When a log reaches `max_size` it is copied to `<name>__YYYY-MM-DD_HH-mm-ss.log` and the original is truncated; only the newest `keep` copies stay. A few lines written between the copy and the truncate can be lost.

What each command promises is in [`docs/specs/`](docs/specs/).

## Install

Needs Rust 1.88 or later.

```fish
git clone https://github.com/plimeor/hum
cd hum
cargo install --path .
```

`cargo install` puts hum at `~/.cargo/bin/hum`, and that is the path the plists record. Reinstalling to the same place keeps services working. If you install somewhere else, run `hum apply` once; `hum doctor` reports this case too.

### Completion

Completion calls hum itself to list service names, so generate the script each time the shell starts rather than saving it to a file, which can break after upgrading hum.

```fish
# ~/.config/fish/config.fish
hum completion fish | source
```

```zsh
# ~/.zshrc
source <(hum completion zsh)
```

```bash
# ~/.bashrc
source <(hum completion bash)
```

## License

Licensed under either of [Apache License, Version 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT), at your option.
