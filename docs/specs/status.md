# 查看服务状态
Type: feature

## Purpose

`hum ls` 列出所有服务及其状态，`hum show` 展示单个服务解析后的配置、生效的环境变量和 launchd 状态。由 app 与 CLI 模块拥有。

## Requirements

### Requirement: 服务状态
每个服务必须显示为以下状态之一：`running`（正在运行）、`disabled`（已被 stop）、`crashed`（未运行且上次非零退出或被信号终止）、`stopped`（其他未运行情况）。

### Requirement: ls 输出
`hum ls` 必须为每个有配置文件或有 hum plist 的服务输出一行，列为 NAME、STATE、PID、LAST EXIT、COMMAND；配置文件无效的服务也必须列出并显示错误。

### Requirement: 标出与 launchd 不一致
配置与 launchd 不一致时（plist 缺失或过期、plist 没有对应的配置、或任务从其他路径加载），`hum ls` 必须在该服务上加标记，并提示运行 `hum apply`。

### Requirement: show 输出
`hum show <name>` 必须输出合并后的完整配置（包括生效的日志轮转设置）、launchd 状态，以及每个生效的环境变量和设置它的那一层（基础变量、全局 `[env]`、哪个 env 文件、服务 `[env]`）。

### Requirement: 环境变量值默认遮蔽
`hum show` 必须默认遮蔽所有环境变量的值，只有给出 `--reveal` 时才显示。

### Requirement: show 报告环境问题
`hum show` 必须标出 env 文件中未定义的变量引用（文件与行号），并在 env 文件缺失时说明 `hum run` 会拒绝启动；这些问题不得阻止其余内容的输出。

### Requirement: JSON 输出
给出 `--json` 时，`hum ls` 与 `hum show` 必须输出同样内容的 JSON，`show` 的遮蔽规则不变。
