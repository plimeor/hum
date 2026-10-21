# hum

把常驻后台命令注册成 macOS 的 launchd 服务，用一个目录里的 TOML 文件统一管理。

`services/` 下每个 `<name>.toml` 就是一个服务，文件名即服务名。常驻、崩溃重启、登录自启都交给 launchd，hum 自己没有常驻进程。launchd 启动的是 `hum run <name>`，前台调试也用它，所以手动跑和托管跑走的是同一条路径。改了命令、目录或环境变量，`hum restart` 就生效，不用重新注册。

只支持 macOS（arm64 / x64）。目前还没有发布版本，需要从源码安装；发布前在 Mac 上的集成验证还没做完。

## 用起来是什么样

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

第一次运行时，hum 会创建全局配置，并把当前 shell 的 `PATH` 写进去，因为 launchd 给服务的 `PATH` 很短。

## 用法

### 服务配置

```toml
# ~/.config/hum/services/t3.toml
command = ["t3", "server"]   # argv 数组；写成字符串时用 /bin/sh -c 执行
cwd = "~/work/t3"
env_file = [".env"]          # 相对 cwd；多个时后者覆盖前者
autostart = true             # 登录时自动启动
restart = "always"           # always | on-failure | never

[env]
PORT = "3773"

[log]
max_size = "10MB"            # 不写时继承全局配置
keep = 3
```

| 字段 | 默认 | 说明 |
|---|---|---|
| `command` | 必填 | 数组按 argv 直接执行，字符串经 `/bin/sh -c` 执行 |
| `cwd` | 用户主目录 | 支持 `~` 和 `$HOME` |
| `env_file` | `[]` | 任一文件缺失时 `hum run` 拒绝启动 |
| `autostart` | `true` | 登录时是否启动 |
| `restart` | `always` | `on-failure` 只在非零退出后重启 |
| `[env]` | `{}` | 值按字面量处理，不展开变量 |
| `[log]` | 继承全局 | `max_size` 按 1024 进位，`keep` 是保留的历史份数 |

未知字段和非法取值会直接报错，整份配置都不会被部分采用。全局配置 `~/.config/hum/config.toml` 只有 `[env]` 和 `[log]` 两个表，所有服务共享。设置 `$HUM_HOME` 可以把配置目录换到别处。

### 环境变量

`hum run` 不继承调用者的环境，而是从零组装。优先级从低到高：

1. 当前进程的 `HOME`、`USER`、`SHELL`、`TMPDIR`
2. 全局 `[env]`
3. 服务的 `env_file`，按列出的顺序
4. 服务的 `[env]`

`.env` 文件里的 `${VAR}` 和 `$VAR` 会展开，只能引用更低的层和同一文件里上方的行。未定义的变量展开为空字符串，`hum show` 会把这类引用标出来。单引号包裹的值不展开，`\$` 得到字面的 `$`。

### 命令

| 命令 | 作用 |
|---|---|
| `hum new <name> [flags] -- <cmd> [args...]` | 新建服务并启动。参数：`--cwd`、`--env K=V`、`--env-file PATH`、`--restart`、`--no-autostart`、`--no-start` |
| `hum edit <name>` / `hum edit --global` | 用 `$VISUAL` / `$EDITOR`（都没设置时用 `vi`）编辑，校验通过才保存 |
| `hum set <name> <key>=<value>...` | 修改字段，保留文件里的注释，例如 `hum set t3 env.PORT=4000 restart=on-failure` |
| `hum set <name> command -- <cmd> [args...]` | 修改 `command` |
| `hum unset <name> <key>...` | 删除字段 |
| `hum rm <name>...` | 停止服务并删除 plist 和配置；`--keep-config` 只注销 |
| `hum start <name>...` | 启动，并解除 `stop` 造成的禁用 |
| `hum stop <name>...` | 停止，之后登录也不再启动，直到 `start` |
| `hum restart <name>...` | 按当前配置重新生成 plist 并重启 |
| `hum ls` | 列出所有服务和状态（running / stopped / crashed / disabled） |
| `hum show <name>` | 合并后的配置、每个环境变量的来源和 launchd 状态；值默认遮蔽，`--reveal` 显示 |
| `hum logs <name>...` | 默认显示最后 100 行；`-n` 指定行数，`-f` 跟踪，`--rotate` 立即轮转 |
| `hum apply` | 按 `services/` 目录对齐 launchd，直接改了目录里的文件后用它；`--dry-run` 只列出改动 |
| `hum doctor` | 检查常见问题，发现错误时以非零状态退出 |
| `hum run <name>` | 前台运行服务，也是 launchd 的入口 |
| `hum completion fish\|zsh\|bash` | 输出补全脚本 |

带 `<name>...` 的命令都接受多个服务名，或者用 `--all` 表示全部服务。全局参数 `--json` 让 `ls` 和 `show` 输出 JSON，`-y` 跳过确认。

用 `edit`、`set`、`unset` 改了服务配置后，正在运行的服务会自动重启；`--no-restart` 可以关掉这一行为。全局配置改动后不会重启任何服务，要立即生效就执行 `hum restart --all`。

### 文件位置

| 内容 | 路径 |
|---|---|
| 服务配置 | `~/.config/hum/services/<name>.toml` |
| 全局配置 | `~/.config/hum/config.toml` |
| launchd plist（由 hum 生成） | `~/Library/LaunchAgents/hum.<name>.plist` |
| 日志（stdout 与 stderr 合并） | `~/Library/Logs/hum/<name>.log` |

launchd 里的 label 都是 `hum.<name>`，可以用 `launchctl list | grep hum.` 直接查看。

日志达到 `max_size` 后会被复制成 `<name>__YYYY-MM-DD_HH-mm-ss.log`，原文件随即截断，只保留最新的 `keep` 份。复制和截断之间写入的少量行可能丢失。

每条命令具体承诺什么，见 [`docs/specs/`](docs/specs/)。

## 安装

需要 Rust 1.88 或更新的版本。

```fish
git clone https://github.com/plimeor/hum
cd hum
cargo install --path .
```

`cargo install` 会把 hum 装到 `~/.cargo/bin/hum`，plist 里记录的就是这个路径。之后在同一位置重新安装，服务照常能用。如果换了安装位置，执行一次 `hum apply`，`hum doctor` 也会提示这类问题。

### 补全

补全时会调用 hum 本身来列出服务名，所以补全脚本要每次启动 shell 时重新生成，不要保存成文件，否则升级 hum 后可能失效。

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
