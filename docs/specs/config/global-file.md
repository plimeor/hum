# 全局配置文件
Type: format

## Purpose

配置目录下的 `config.toml` 存放所有服务共享的环境变量和日志轮转默认值。由配置模块拥有。

## Fields
| Field | Type | Default | When invalid |
|---|---|---|---|
| `[env]` | 字符串到字符串的表，值按字面量处理 | `{}` | 值不是字符串、或键不符合 `[A-Za-z_][A-Za-z0-9_]*` 时文件无效 |
| `[log].max_size` | 大小字符串，格式同服务配置 | `"10MB"` | 无法解析或不为正数时文件无效 |
| `[log].keep` | 非负整数 | `3` | 负数时文件无效 |

## Requirements

### Requirement: 严格解析
hum 必须拒绝含未知字段、类型不符或取值非法的全局配置；文件不存在时必须视为所有字段取默认值。

### Requirement: 首次运行时创建
`config.toml` 不存在时，除 `hum run`、帮助和补全外的任何 hum 命令都必须创建它，并把当时 shell 的 `PATH` 写入 `[env]`。

#### Scenario: launchd 启动不会写入短 PATH
- **WHEN** 配置目录里还没有 `config.toml`，launchd 执行 `hum run t3`
- **THEN** hum 不创建 `config.toml`

### Requirement: 服务配置覆盖全局日志设置
服务配置中的 `[log].max_size` 与 `[log].keep` 必须逐项覆盖全局值，未设置的项必须取全局值。
