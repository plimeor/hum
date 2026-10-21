# 运行服务
Type: feature

## Purpose

`hum run <name>` 读配置、组装环境、切换目录，然后把自己替换成服务命令。launchd 托管和前台手动调试走同一条路径。由 run 与环境组装模块拥有。

## Requirements

### Requirement: 替换为服务进程
`hum run <name>` 必须在 `cwd` 中以 exec 方式把自身替换为服务命令，不得作为父进程留在进程链中；服务命令的退出状态即该进程的退出状态。

### Requirement: launchd 运行同一入口
hum 生成的 launchd 任务必须运行 `hum run <name>`，而不是服务命令本身，使 `command`、`cwd` 和环境变量的改动在下次启动时生效，不需要重新注册。

### Requirement: 环境从零组装
`hum run` 不得继承调用者的环境，只能组装以下各层，优先级从低到高：基础变量 `HOME`、`USER`、`SHELL`、`TMPDIR`（取自当前进程）；全局 `[env]`；服务的 `env_file`，按列出顺序；服务的 `[env]`。

#### Scenario: 调用者的变量不会泄漏
- **WHEN** 用户在设置了 `LANG=zh_CN.UTF-8` 的 shell 中执行 `hum run t3`，且全局与服务配置都未设置 `LANG`
- **THEN** 服务进程的环境中没有 `LANG`

### Requirement: 用组装后的 PATH 查找命令
命令的第一个参数不含 `/` 时，必须在组装后环境的 `PATH` 中查找；找不到时必须以非零状态退出，不启动服务。

### Requirement: env 文件缺失时拒绝启动
`env_file` 中任一文件不存在或无法读取时，`hum run` 必须以非零状态退出、不启动服务，并把缺失的路径写到标准错误（launchd 托管时即该服务的日志）。

### Requirement: 启动前检查日志大小
`hum run` 在启动服务前必须检查该服务的日志，达到 `max_size` 时先轮转。
