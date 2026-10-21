# Project overview
Updated 2026-10-21

## What this is

hum 是一个用 Rust 写的 macOS CLI，把常驻后台命令（如 `t3 server`）注册成 launchd 服务，并用一个目录里的 TOML 文件统一管理它们。

## Principles

- **一个文件就是一个服务**：和 fish 的 `functions/` 一样，`services/` 下每个 `<name>.toml` 就是一个服务，文件名即服务名。文件是唯一事实来源，plist 是生成物。
- **托管交给 launchd**：常驻、崩溃重启、登录自启都由系统负责，hum 没有自己的常驻 daemon。
- **手动跑和托管跑行为一致**：launchd 启动的是 `hum run <name>`，前台调试用的也是它，避免“手动能跑、托管就挂”。
- **改配置不用重新注册**：命令、目录、环境变量都在启动时读取，restart 即生效。

## Status
Shipped features are described in `docs/specs/`.

## Current focus

v1：只支持 macOS（arm64 / x64），实现全部 14 个命令。代码与测试已完成；launchd 以 `O_APPEND` 打开日志、copytruncate 轮转可行，已在 Mac 上实测。剩余的是在真实 Mac 上完成验证计划：

- 编译 Rust 版并跑真实 launchd 测试：`cargo test -- --ignored --test-threads=1 launchd::macos`。
- 本机集成：`hum new sleeper -- sleep 1000` 后 `ls` 显示 running；`kill -9` 后约 10 秒内被拉起；`stop` 后不再拉起；`rm` 后 plist 和配置都消失。
- 环境变量：launchd 托管运行和 `hum run` 前台运行的环境一致。
- 登录与升级：注销重新登录后 `autostart = true` 的服务自动启动、已 `stop` 的不启动；brew 升级 hum 后服务仍能正常重启。

## Next

以上全部通过，且 `t3 server` 作为第一个真实服务稳定跑满一周，即发布 v0.1.0。

## Non-goals

- Linux / systemd 支持
- 健康检查、服务间依赖、定时任务、资源限制
- TUI 面板
- 以 root 身份运行的系统级 daemon（`/Library/LaunchDaemons`）
