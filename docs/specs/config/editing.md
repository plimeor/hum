# 新建与修改配置
Type: feature

## Purpose

用户通过 `hum new`、`hum edit`、`hum set`、`hum unset` 创建和修改服务配置，改动校验通过后才落盘，并按规则生效。由 CLI 模块拥有。

## Requirements

### Requirement: 新建服务
`hum new <name> [flags] -- <cmd> [args...]` 必须把 `--` 之后的参数原样写成 `command` 数组，`cwd` 默认取当前目录，并支持 `--cwd`、可重复的 `--env K=V` 与 `--env-file PATH`、`--restart`、`--no-autostart`；除非给出 `--no-start`，必须随即启动服务。

### Requirement: 新建不覆盖
同名服务已存在时，`hum new` 必须报错退出，且不得修改已有配置。

### Requirement: 新建时只警告不阻止
`command` 的第一个参数在全局 `PATH` 中找不到、或 `env_file` 指向的文件不存在时，`hum new` 必须给出警告，仍然写入配置。

### Requirement: 在编辑器中修改
`hum edit <name>` 与 `hum edit --global` 必须在临时副本上打开 `$VISUAL`，未设置时用 `$EDITOR`，都未设置时用 `vi`；保存后校验，失败时必须提示错误并询问是否重新编辑，原文件保持不变；校验通过后必须原子替换原文件。

### Requirement: 命令行修改字段
`hum set <name> <key>=<value>...` 与 `hum unset <name> <key>...` 必须只接受 `command`、`cwd`、`env_file`、`autostart`、`restart`、`env.<NAME>`、`log.max_size`、`log.keep`；`env_file` 的值以逗号分隔多个路径；`command` 必须通过 `hum set <name> command -- <cmd> [args...]` 设置，且不能被 unset。

### Requirement: 修改保留注释
`hum set` 与 `hum unset` 必须只改动目标键所在的内容，文件中其余文本（包括注释）必须保持不变。

### Requirement: 无效结果不落盘
`hum set` 或 `hum unset` 的结果不是有效配置时，必须报错退出，且不得修改文件。

### Requirement: 服务配置改动的生效
服务配置经 `edit`、`set` 或 `unset` 改动后，如果服务正在运行，hum 必须自动 restart 它；给出 `--no-restart` 时不得重启，并提示需要 restart 才能生效。

### Requirement: 全局配置改动不重启
`hum edit --global` 改动后不得重启任何服务，必须输出一行提示：改动会在各服务下次重启后生效。
