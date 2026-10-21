# 服务配置文件
Type: format

## Purpose

`services/<name>.toml` 描述一个服务：运行什么、在哪运行、带什么环境、是否登录自启、退出后是否重启。它是服务的唯一事实来源，launchd plist 由它生成。由配置模块拥有。

## Fields
| Field | Type | Default | When invalid |
|---|---|---|---|
| `command` | 字符串数组（argv），或字符串（经 `/bin/sh -c` 执行） | 必填 | 缺失、空数组、空字符串或含非字符串元素时整个文件无效 |
| `cwd` | 路径字符串，支持 `~` 与 `$HOME` | 空，此时为用户主目录 | 类型不符时文件无效；目录不存在不影响解析，`hum run` 时失败 |
| `env_file` | 路径字符串数组，相对 `cwd`，支持 `~` 与 `$HOME` | `[]` | 含空字符串时文件无效 |
| `autostart` | 布尔 | `true` | 类型不符时文件无效 |
| `restart` | `always` \| `on-failure` \| `never` | `always` | 其他值时文件无效 |
| `[env]` | 字符串到字符串的表，值按字面量处理 | `{}` | 值不是字符串、或键不符合 `[A-Za-z_][A-Za-z0-9_]*` 时文件无效 |
| `[log].max_size` | 大小字符串，如 `"10MB"`；单位 `B`/`K`/`KB`/`M`/`MB`/`G`/`GB`，按 1024 进位 | 继承全局配置 | 无法解析或不为正数时文件无效 |
| `[log].keep` | 非负整数 | 继承全局配置 | 负数时文件无效 |

## Requirements

### Requirement: 文件名即服务名
服务配置必须位于配置目录的 `services/<name>.toml`，服务名取自文件名，且必须匹配 `[a-z0-9][a-z0-9_-]*`。

### Requirement: 配置目录位置
配置目录必须默认为 `~/.config/hum`；设置了 `$HUM_HOME` 时必须改用它，且 launchd 启动的 `hum run` 必须读取同一目录。

### Requirement: 严格解析
hum 必须拒绝含未知字段、类型不符或取值非法的配置文件，并在错误中指出出错的字段或位置；无效文件不得被部分采用。

### Requirement: 字符串形式的 command
`command` 为字符串时，hum 必须以 `/bin/sh -c <command>` 运行它；为数组时，必须不经 shell 直接运行该 argv。

### Requirement: 路径展开
`cwd` 与 `env_file` 中开头的 `~` 以及 `$HOME`、`${HOME}` 必须展开为用户主目录，相对的 `env_file` 必须相对 `cwd` 解析；`[env]` 的值必须按字面量处理，不做任何展开。
