# 日志
Type: feature

## Purpose

每个服务的输出写入一个日志文件，`hum logs` 查看、跟踪或立即轮转它，hum 自动轮转超过大小的日志。由 logfile 模块拥有。

## Requirements

### Requirement: 日志位置
每个服务的标准输出与标准错误必须合并写入 `~/Library/Logs/hum/<name>.log`。

### Requirement: 查看日志
`hum logs <name>...` 必须默认显示每个日志的最后 100 行，`-n` 指定行数；同时查看多个服务或给出 `--all` 时，每行必须带 `[name]` 前缀。

### Requirement: 跟踪日志
`hum logs -f` 必须持续输出新写入的行，直到被中断；日志被轮转截断后必须从新内容继续输出。

### Requirement: 自动轮转
日志达到生效的 `max_size` 后，hum 必须在不需要用户操作的情况下轮转它；检查的时间间隔不作承诺。

### Requirement: 轮转方式
轮转必须把当前日志复制为同目录下的 `<name>__YYYY-MM-DD_HH-mm-ss.log`（时间为本地时间，同一秒内重复轮转时加 `_1`、`_2` 后缀），再把原文件截断为空，不得重命名或替换原文件；只保留最新的 `keep` 份副本，更早的必须删除。复制与截断之间写入的行可能丢失。

#### Scenario: 正在写日志的服务不受影响
- **WHEN** 服务 `t3` 正在运行，其日志被轮转
- **THEN** `t3` 之后的输出继续写入 `~/Library/Logs/hum/t3.log`，无需重启

### Requirement: 空日志不轮转
空日志不得被轮转，也不得产生副本。

### Requirement: 立即轮转
`hum logs <name>... --rotate` 必须立即轮转指定日志，不看大小；日志为空时必须说明无需轮转。
