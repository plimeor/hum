# 服务生命周期
Type: feature

## Purpose

`hum start`、`stop`、`restart`、`rm` 通过 launchd 控制服务的运行、登录自启和崩溃重启。由 app 模块拥有，launchd 操作封装在 launchd 模块后面。

## Requirements

### Requirement: launchd 任务的位置与标签
每个服务必须注册为当前用户的 LaunchAgent，label 为 `hum.<name>`，plist 位于 `~/Library/LaunchAgents/hum.<name>.plist`；plist 只由 hum 生成。

### Requirement: 重启策略
`restart = "always"` 的服务必须在每次退出后被 launchd 重新拉起；`"on-failure"` 必须只在非零退出后被拉起；`"never"` 不得被拉起。两次拉起之间的间隔由 launchd 决定。

### Requirement: 登录自启
`autostart = true` 且未被 stop 的服务必须在用户登录时启动；`autostart = false` 的服务登录时不得启动。

### Requirement: start
`hum start` 必须解除 stop 造成的禁用、按当前配置重新生成 plist 并启动服务，`autostart = false` 的服务也要启动；服务已在运行时不得做任何操作。

### Requirement: stop
`hum stop` 必须停止服务，并使其在之后的登录中也不启动，直到执行 `hum start` 或 `hum restart`。

### Requirement: restart
`hum restart` 必须按当前配置重新生成 plist 并重启服务，使配置与 `.env` 的改动在此时生效；已被 stop 的服务也要重新启用并启动。

### Requirement: rm
`hum rm` 必须停止服务、删除其 plist，再删除配置文件；除非给出 `-y`，必须先请求确认；给出 `--keep-config` 时只注销，保留配置文件。之后新建的同名服务不得继承被删服务的 stop 状态。

### Requirement: plist 中的 hum 路径
写入 plist 的 hum 路径必须在 PATH 上存在指向当前二进制的稳定路径（如包管理器的符号链接）时优先使用它，而不是升级后会失效的版本化路径。
