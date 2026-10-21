# 0001: 用 Rust 实现 hum

Status: Accepted
Date: 2026-10-21

## Context

hum 先用 Go 实现了全部 14 个命令。用户的判断是：代码越少，出错的概率通常越低，因此想知道换一种语言能少多少代码。比较时有两个前提：

- hum 有两块逻辑必须手写，与语言无关：`hum set` / `unset` 修改配置时保留注释；`.env` 展开变量时要查到更低优先级的层（基础变量、全局 `[env]`、前面的 env 文件），并报告未定义的引用。
- `hum run` 要用 exec 把自身替换成服务进程，见 `docs/specs/run.md`。

## Decision

hum 用 Rust 实现，取代 Go 版本。

## Considered options

- **Go（原实现）**：cobra 自带子命令、帮助和补全。主要的额外代码来自显式错误处理：生产代码中约有 340 处错误检查。保留注释的 TOML 编辑是手写的，共 416 行。
- **Bun / TypeScript**：`Bun.TOML.stringify` 从普通对象生成文本，会丢掉注释；`util.parseEnv` 不展开变量。所以两块必须手写的逻辑都省不掉。`util.parseArgs` 没有子命令和补全。`bun build --compile` 编出的 hello world 有 77.5 MB。`process.execve` 在 Bun 1.4.2 的 Linux 上可用，macOS 上未验证。否决的理由是：省下的主要是错误处理样板，难点一样不少，单文件分发的体积却大一个数量级。
- **Rust（选定）**：
  - `toml_edit` 保留注释，手写编辑缩减到约 60 行；
  - serde 的 `deny_unknown_fields` 和枚举直接表达严格解析；
  - clap 和 clap_complete 提供子命令与动态补全；
  - 标准库自带 exec；
  - `dotenvy` 只查进程环境、不报告未定义引用，所以 `.env` 解析仍然手写。

## Consequences

- 生产代码（不含空行、注释，按各自默认格式化）从 Go 的 3,508 行降到 Rust 的 2,769 行，约少 21%。release 二进制从 5.1 MB 降到 2.9 MB。
- 省下的行数大多来自错误处理和 TOML 编辑；分层 env 展开等逻辑与语言无关，仍需自己写对。
- 动态补全依赖 `clap_complete` 的 `unstable-dynamic` 特性，升级该依赖时需要核对补全是否仍然可用。
- 发布前必须在 Mac 上重新验证，因为 Go 版的验证结论不能直接沿用到 Rust 版。
