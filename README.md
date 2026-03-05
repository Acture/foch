# Foch

`foch-cli` 是一个 Paradox Mod Playset 静态分析工具。当前版本提供通用规则引擎，会构建脚本符号索引并校验 playset 数据完整性、mod 描述文件、文件覆盖冲突、依赖关系以及 scripted effects 的定义/引用一致性。

## 安装

```bash
cargo build
```

## 快速开始

```bash
# 查看帮助
cargo run -- --help

# 检查 playset
cargo run -- check ./playlist.json

# 严格模式（有 strict finding 则返回退出码 2）
cargo run -- check ./playlist.json --strict

# 输出 JSON
cargo run -- check ./playlist.json --format json --output result.json

# 语义分析模式（默认 semantic）
cargo run -- check ./playlist.json --analysis-mode semantic

# 仅输出 strict 通道
cargo run -- check ./playlist.json --channel strict

# 导出语义图
cargo run -- check ./playlist.json --graph-out semantic.dot --graph-format dot
```

## 配置

配置文件默认在 `~/.config/foch/config.toml`。

可通过环境变量覆盖配置目录：

```bash
export FOCH_CONFIG_DIR=/tmp/foch-config
```

配置命令示例：

```bash
cargo run -- config show
cargo run -- config show --json
cargo run -- config validate
cargo run -- config set steam-path /path/to/steam
cargo run -- config set paradox-data-path /path/to/paradox
cargo run -- config set game-path eu4 /path/to/game
```

## 退出码

- `0`: 成功（无系统错误；非 strict 模式下 finding 不影响退出码）
- `1`: 系统错误（例如文件不可读）
- `2`: `--strict` 且存在 strict findings

## 开发质量闸门

```bash
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

## EU4 内建符号表

仓库内置 `src/check/data/eu4_builtin_catalog.json`，用于识别内建 trigger/effect，降低把引擎内建语句误判为 scripted effect 调用的概率。

如需重建该表（CWTools + eu4wiki 镜像 + 本机 EU4 文件频次）：

```bash
python3 scripts/build_eu4_builtin_catalog.py
```

默认会读取 `/tmp/foch-sources` 下缓存资料，并自动探测本机 EU4 目录。可通过 `FOCH_EU4_PATH` 覆盖。
