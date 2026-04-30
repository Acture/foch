# Foch

`foch` 是一个面向 Paradox mod playset 的分析与合并工具包。当前版本已经提供 `check`、`merge-plan`、`merge`、`graph`、`simplify`、`data` 与 `config` 命令；其中 `merge-plan` / `merge` 可以生成并复验 EU4 merged mod 输出，而 `check` / `graph` / `simplify` 继续承担分析、可视化与清理工作流。

Additional documentation lives in [`docs/`](./docs/README.md):

- [`docs/project-status.md`](./docs/project-status.md): current repository status, verified checks, and completion estimates under different goal definitions
- [`docs/auto-merge-roadmap.md`](./docs/auto-merge-roadmap.md): milestone-oriented roadmap from analyzer foundation to auto-merge workflow
- [`docs/merge-design.md`](./docs/merge-design.md): implementation-grade merge specification for commands, artifact layout, strategies, and validation

## 安装

```bash
# 从 crates.io 安装
cargo install foch

# 或本地构建
cargo build --bin foch
```

## 快速开始

以下假设你在仓库根目录，并已经准备好 Paradox Launcher 导出的 `playlist.json`。把 EU4 路径替换成你的本机安装目录。

```bash
# 1. 安装当前 alpha CLI（暂未发布到 crates.io）
cargo install --path apps/foch-cli

# 2. 配置 EU4 基础游戏目录
foch config set game-path eu4 "/path/to/Europa Universalis IV"

# 3. 分析 playset：解析脚本、构建语义索引、检查跨 mod overlap / D1 / V2
foch check ./playlist.json

# 4. 生成 deterministic merged mod 目录
foch merge ./playlist.json --out ./merged
```

常见成功输出形态：

> `Foch Check Report`
> `fatal_errors: 0`
> `strict_findings: 0`

> `Foch Merge Report`
> `status: READY`
> `manual_conflict_count: 0`

如果默认 merge 报告 unresolved conflicts，按风险从低到高选择一种继续方式：

```bash
# TTY 下逐个仲裁，并把决定写入 foch.toml；非 TTY 会自动 defer
foch merge ./playlist.json --out ./merged --interactive

# 或手写 foch.toml [[resolutions]] 后重跑默认 merge
foch merge ./playlist.json --out ./merged

# 或显式接受 last-writer fallback（可写冲突标记时会写入 marker）
foch merge ./playlist.json --out ./merged --fallback
```

## 结构化 merge 支持范围

从 post-F6c 开始，EU4 `GameProfile` 通过 `ContentFamilyDescriptor` 注册的内容族默认进入结构化 merge：解析、语义索引、DAG 排序和 level-by-level patch apply 使用同一套 mod → vanilla 依赖图，不再把跨 base 的 diff artifact 扁平化到同一层。

README 不再手工枚举内容族，避免与实现漂移。当前 canonical 列表在 [`crates/foch-language/src/analyzer/eu4_profile.rs`](./crates/foch-language/src/analyzer/eu4_profile.rs) 的 `EU4_CONTENT_FAMILIES`；新增 EU4 root 时以该文件中的 `ContentFamilyDescriptor` 为准。

残余的 sibling-overwrite、replace-block、true list-item rename 等结构性分歧需要用户仲裁。foch 不再静默丢弃这类贡献：默认输出会显式阻塞 / 跳过相关路径；只有用户配置 `[[resolutions]]`、使用 `--interactive`，或显式传入 `--fallback` 时才会继续。

## 冲突仲裁工作流

默认 `foch merge ./playlist.json --out ./merged` 遇到无法安全选择的结构冲突时，会把相关输出路径跳过，并在 `./merged/.foch/foch-merge-report.json` 写入 `conflict_resolutions[]`。此时可选：用 `--interactive` 在 TTY 中逐个选择；手写 `foch.toml [[resolutions]]` 后重跑；或用 `--fallback` 生成 last-writer 输出。

`foch.toml` 的 `[[resolutions]]` 每条规则只能选一个 selector 和一个 action。字段速查（YAML 风格；不要把所有字段放进同一个 TOML block）：

> `file: "events/PirateEvents.txt"` — 按输出路径匹配
> `conflict_id: "ab12cd34"` — 按具体结构冲突匹配
> `prefer_mod: "3378403419"` — 选某个 mod 的 patch
> `use_file: "manual/events/PirateEvents.txt"` — 用外部文件替换输出
> `keep_existing: true` — 保留 out 目录已有文件
> `priority_boost: 100` — 给某个 `mod` 增加局部优先级

可直接粘贴的 TOML 示例：

```toml
[[resolutions]]
conflict_id = "ab12cd34"
prefer_mod = "3378403419"

[[resolutions]]
file = "events/PirateEvents.txt"
use_file = "manual/events/PirateEvents.txt"

[[resolutions]]
file = "common/ideas/00_country_ideas.txt"
keep_existing = true

[[resolutions]]
mod = "3378403419"
priority_boost = 100
```

`--interactive` 的选择项是候选 mod 编号、`d` defer、`s` use file path、`k` keep existing、`q` abort；确认后会把可持久化决定追加到 `foch.toml`。如果 stdin/stderr 不是 TTY，interactive 会自动降级为 defer，不会卡住 CI。

`conflict_id` 是稳定哈希：输入为报告里的 `conflict_resolutions[].path`（输出文件路径）加该冲突的结构地址（interactive 输出中的 `address:`，即 address path + key）。同一个 id 可直接写回 `foch.toml` 的 `conflict_id` selector。

一次典型闭环：先运行 merge，看 `Foch Merge Report` 和 `.foch/foch-merge-report.json`；定位到 `events/PirateEvents.txt` 的冲突后，在仓库根目录写入一个 `[[resolutions]]`：

```toml
[[resolutions]]
conflict_id = "ab12cd34"
prefer_mod = "3378403419"
```

然后重跑：

```bash
foch merge ./playlist.json --out ./merged
```

期望输出回到：

> `status: READY`
> `manual_conflict_count: 0`

## 配置

配置分成两个 schema，路径相近但用途不同：

- `~/.config/foch/config.toml` 是 `foch_engine::Config`：`steam_root_path`、`paradox_data_path`、`game_path` map、`extra_ignore_patterns`。它由 `foch config set` / `show` / `validate` 管理，通常不手写。
- 项目根目录或 playset 旁边的 `foch.toml`，以及用户级 `~/.config/foch/foch.toml`，是 `foch_core::FochConfig`：手写 `[[overrides]]`（D2，本地忽略错误依赖边）、`[[resolutions]]`（R1，冲突仲裁）和 `[emit] indent`（合并输出缩进，默认 tab）。`foch merge --config PATH` 可显式指定该文件。

可通过环境变量覆盖 engine 配置目录：

```bash
export FOCH_CONFIG_DIR="$HOME/.config/foch-alpha"
```

配置命令示例：

```bash
foch config show
foch config show --json
foch config validate
foch config set steam-path /path/to/steam
foch config set paradox-data-path /path/to/paradox
foch config set game-path eu4 "/path/to/Europa Universalis IV"
```

## 当前噪音水平 (alpha)

最新 N=37 EU4 probe baseline：post-F1b-comments（HEAD `8c5aa66`）仍有 26 个 unresolved structural conflicts，低于 F4 baseline / pre-F6c 的 181。

这 26 个 residual 分解为 20 个 sibling overwrite、5 个 replace-block、1 个真实 list-item rename。它们都是跨 mod 内容分歧，需要用户仲裁；不是 comment diff 或 DAG flattening 造成的 foch false positive。完整限制清单见后续维护的 [`KNOWN_LIMITATIONS.md`](./KNOWN_LIMITATIONS.md)。

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

JS workspace（`packages/tree-sitter-paradox`、`packages/vscode-foch`）默认使用仓库根目录的 `.envrc` 把 Homebrew 的 `node@22` 放到 `PATH` 前面。先执行 `brew install node@22`，然后在仓库根目录运行一次 `direnv allow`。当前要求 `node >=22 <25` 与 Bun 1.2+；`node@25` 下 `tree-sitter` 原生依赖当前无法稳定构建，不属于支持环境。

## 发布自动化

仓库内置四条 GitHub Actions 工作流：

- `ci.yml`: Rust 质量闸门 + `tree-sitter-paradox` / VS Code 扩展 smoke
- `release.yml`: 为 tag 构建 CLI 压缩包、VSIX、带 submodule 内容的 source tarball，发布 GitHub Release，并同步 Homebrew tap
- `publish.yml`: 手动从现有 GitHub Release source asset 重新同步 Homebrew tap
- `publish-vscode-preview.yml`: 手动发布 VS Code preview 扩展

发布所需的 GitHub secrets / variables：

- `VSCE_PAT`: VS Code Marketplace token
- `HOMEBREW_TAP_TOKEN`: 用于推送 tap 仓库
- `HOMEBREW_TAP_REPO`: repository variable，例如 `Acture/homebrew-tap`

说明：

- Homebrew formula 现在使用 `release.yml` 产出的 source tarball，而不是 GitHub 自动生成的 tag archive；这样 source 包会包含 `packages/tree-sitter-paradox` submodule 内容
- `publish.yml` 只是 Homebrew tap 的手动重同步入口，不承担 crates.io 发布

## 解析缓存（本地）

`check` / `merge-plan` 现在会缓存两层本地数据：

- 文件级 parser cache（game + mod 通用，位于系统 cache 目录）
- mod semantic snapshot cache（按 `game + mod identity + manifest hash` 命中，位于系统 cache 目录）

基础游戏不再走隐式本地扫描缓存。默认行为是加载已安装的 base data；缺失时需要显式运行：

- `foch data install eu4 --game-version auto`
- `foch data build eu4 --from-game-path /path/to/eu4 --game-version auto --install`

可选环境变量：

```bash
export FOCH_PARSE_CACHE_DIR=/tmp/foch-parse-cache
export FOCH_MOD_SNAPSHOT_CACHE_DIR=/tmp/foch-mod-snapshot-cache
export FOCH_DATA_DIR=/tmp/foch-data
```

## 真实语料解析统计（本地工具）

```bash
# 统计某目录下 .txt 解析成功率
cargo run --bin parse_stats -- "/path/to/eu4" --exts txt

# 排除非脚本文本目录（例如 license / patchnotes）
cargo run --bin parse_stats -- "/path/to/eu4" --exts txt --exclude-prefixes licenses,patchnotes
```

## 真实语料 smoke 对比（本地工具）

```fish
python3 scripts/eu4_real_smoke.py --playset /path/to/playset.json --out-dir target/eu4-real-smoke/baseline
python3 scripts/eu4_real_smoke.py --playset /path/to/playset.json --out-dir target/eu4-real-smoke/act-32-post

python3 scripts/eu4_real_smoke_compare.py \
	target/eu4-real-smoke/baseline/<slug>-summary.json \
	target/eu4-real-smoke/act-32-post/<slug>-summary.json \
	--rule S004 \
	--gate-rule S004 \
	--min-absolute-drop 250 \
	--min-relative-drop 0.08
```

这里的 `playset.json` 只是你本机上的实际 playset 路径，不是仓库约定文件名。第一条脚本生成真实语料 summary；第二条脚本输出 rule delta、热点路径变化，并在 gate 失败时返回非零退出码，适合收口 `ACT-32` / `ACT-31` / `ACT-28` 这类真实语料清噪任务。阈值参数按具体 issue 调整，不要把示例值当成固定标准。

## LSP 与 VS Code

项目包含 `foch_lsp` language server，以及位于 `packages/vscode-foch/` 的 VS Code 扩展。

当前已实现：

- `EU4 Script` 文件类型与语法高亮
- reserved/contextual/alias 关键字补全
- builtin trigger/effect 补全
- 工作区符号补全（event id / scripted effect / decision / flag value）
- `goto definition`
  - scripted effect 调用
  - event id 引用
  - flag value 引用
  - localisation key 引用
- 编辑器 diagnostics
  - 当前文档 parse errors
  - 工作区语义 findings（例如 unresolved call / invisible alias / missing localisation / unresolved flag）

当前仍未实现：

- `hover`
- `find references`
- `rename`
- code action

启动方式：

```bash
cargo run --bin foch_lsp
```

VS Code 本地开发：

```bash
cd packages/vscode-foch
bun install
bun run prepare:server
code .
```

然后在 Extension Development Host 里测试扩展。

可选：通过环境变量指定 LSP 仅扫描哪些目录（优先于 workspace folders）：

```bash
export FOCH_LSP_TARGETS_JSON='[
	{"path":"/path/to/Europa Universalis IV","role":"game"},
	{"path":"/path/to/my_mod","role":"mod"}
]'
```

`role` 目前支持 `game` 与 `mod`。

如果不设置 `FOCH_LSP_TARGETS_JSON`，VS Code 扩展会：

- 读取 `fochLsp.gamePath`
- 读取 `fochLsp.modPaths`
- 通过 `descriptor.mod` 自动发现 mod 根目录

语义扫描目录当前覆盖：

- `events/`
- `decisions/`
- `common/scripted_effects/`
- `common/diplomatic_actions/`
- `common/triggered_modifiers/`
- `common/defines/`
- `interface/`
- `common/interface/`
- `gfx/`

其中 UI 目录当前主要用于解析与 diagnostics，不参与完整 scope/symbol 语义推导。

## EU4 内建符号表

仓库内置 `crates/foch-language/src/data/eu4_builtin_catalog.json`，用于识别内建 trigger/effect，降低把引擎内建语句误判为 scripted effect 调用的概率。

如需重建该表（CWTools + eu4wiki 镜像 + 本机 EU4 文件频次）：

```bash
python3 scripts/build_eu4_builtin_catalog.py
```

默认会读取 `/tmp/foch-sources` 下缓存资料，并自动探测本机 EU4 目录。可通过 `FOCH_EU4_PATH` 覆盖。
