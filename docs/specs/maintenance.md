# 对齐与检查
Type: feature

## Purpose

`hum apply` 让 launchd 与配置目录对齐，`hum doctor` 检查常见问题。用户直接在配置目录里增删改文件后，靠 `apply` 生效。由 app 模块拥有。

## Requirements

### Requirement: apply 对齐
`hum apply` 必须以 `services/` 目录为准：为新的配置文件生成 plist 并注册；注销没有配置文件的 hum plist 并删除它；重新生成过期的 plist 并重新加载，重新加载前在运行的服务之后仍在运行。

### Requirement: apply 跳过无效配置
`hum apply` 必须跳过无效的配置文件并逐个报告，继续处理其余服务，最后以非零状态退出。

### Requirement: apply 预演
`hum apply --dry-run` 必须只列出将要进行的改动，不得改动任何文件或 launchd 状态。

### Requirement: doctor 检查项
`hum doctor` 必须检查：全局配置有效且设置了 `PATH`；每个服务配置有效；`command` 能在全局 `PATH` 中找到；`cwd` 存在；`env_file` 都存在；plist 中的 hum 路径存在且可执行，并对升级后会失效的版本化路径给出警告；配置与 launchd 一致；没有对应配置的 hum plist；日志轮转任务已安装；日志异常增大。

### Requirement: doctor 退出状态
`hum doctor` 发现错误级问题时必须以非零状态退出；只有警告时以零退出。
