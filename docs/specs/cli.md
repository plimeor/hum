# 命令行通用约定
Type: feature

## Purpose

所有 hum 命令共享的参数、多服务处理和退出状态约定。由 CLI 模块拥有。

## Requirements

### Requirement: 多个服务名与 --all
`start`、`stop`、`restart`、`rm`、`logs` 必须接受多个服务名或 `--all`（全部有配置文件的服务），二者不能同时给出。`start`、`stop`、`restart`、`rm` 与 `logs --rotate` 中某个服务失败时，必须报告并继续处理其余服务，最后以非零状态退出。

### Requirement: 跳过确认
全局参数 `-y` 必须跳过所有确认提示，并视为同意。

### Requirement: 补全脚本
`hum completion fish|zsh|bash` 必须输出对应 shell 的补全脚本，补全中包含已配置的服务名。

### Requirement: 失败退出状态
任何命令失败时必须向标准错误输出原因，并以非零状态退出。
