# 已知限制

本文记录 post-F6c + F1b-comments alpha 在 `a37ffb8` 附近的真实边界。目标读者是准备把 foch 放进 EU4 merge pipeline 的 power user / mod author。

口径：以 N=37 EU4 playset probe 为主。`conflict_resolutions[]` 是 26 条 path-level residual 记录；单条记录内部可能含多个 AST address 级 conflict。

## 1. 当前合并冲突基线（N=37）

| 阶段 | unresolved residual |
|---|---:|
| post-F4 baseline | 181 |
| post-F6c | 50 |
| post-F1b-comments（HEAD） | 26 |

26 条 residual 的 bucket 口径如下：

| Bucket | 数量 | 含义 | 为什么不自动解 | 现在可做 | 跟踪 |
|---|---:|---|---|---|---|
| sibling-overwrite | 20 | 多个 mod 在同一 AST 地址下改了相邻字段 / 子块，且结果不同。常见于 mission effect、history date block、GUI layout。 | foch 能定位同一结构位置，但不知道作者意图：应合并、择一、还是保持两个互斥版本。静默 last-writer 会丢贡献。 | 用 `--interactive` 逐条选；或在 `foch.toml [[resolutions]]` 里 `prefer_mod` / `use_file` / `keep_existing`；或显式 `--fallback` 接受 last-writer 风险。 | UI1 / family policy refinements |
| replace-block | 5 | 多个 mod 替换同一个命名块，块内容不等价。 | 这是有意重写的强信号。没有内容族规则时，foch 不能证明 BooleanOr / union / recursive merge 安全。 | 手工审阅块语义；为该 conflict 写 resolution；必要时提交更窄的 ContentFamily policy。 | F-family policy work |
| list-item rename | 1 | 当前表现为 `RemoveListItem + AppendListItem`，逻辑上可能是同一列表项被改名 / 改 stable id。 | F1b 只消除了 comment-only diff；还没有按 `name=`、`id=` 等 stable identity key 匹配 rename。 | 手工选择或用 `use_file`；若确认是纯 rename，可在后续 issue 里提供最小 fixture。 | F1b-rename（低 ROI，暂排 UI1 后） |

这些 residual 不是 comment diff，也不是旧 DAG flattening 造成的主要 false positive。默认 `foch merge` 会阻塞 / 跳过相关路径；只有用户明确配置 resolution、启用 `--interactive`，或传入 `--fallback` 时才继续。

## 2. ContentFamily 覆盖

foch 的 analyzer 与 merge 覆盖由 EU4 `GameProfile` 中的 `ContentFamilyDescriptor` 注册驱动。当前 source of truth 是 [`crates/foch-language/src/analyzer/eu4_profile.rs`](./crates/foch-language/src/analyzer/eu4_profile.rs) 的 `EU4_CONTENT_FAMILIES`。

| 限制 | 当前状态 | 现在可做 | 跟踪 |
|---|---|---|---|
| 未注册 root 不会自动获得完整结构语义 | 已注册 family 可进入语义索引、resource extraction、merge-key policy、DAG patch apply；未注册或能力不足的 root 会退回 LastWriterOverlay 或 ManualConflict。 | 不要靠本文枚举 family。运行 `foch check ./playlist.json` 看报告 coverage section，并直接读 `EU4_CONTENT_FAMILIES`。 | coverage reset / ContentFamily slices |
| `ScriptFileKind` 不是扩展点 | 它只是兼容标签。真正行为在 `ContentFamilyDescriptor`：path matcher、scope policy、merge key、symbol kind、extractor、merge policy。 | 新 root 需要注册 descriptor，并补 semantic-index / base-data 覆盖测试。 | ContentFamily promotion |

## 3. F1b-rename 未实现

| 项 | 当前状态 | 用户影响 | 现在可做 | 跟踪 |
|---|---|---|---|---|
| comment-only diff | 已处理。`8c5aa66` 之后，只有注释不同的 AST diff 不再制造 `Remove + Append` 噪音。 | F1b-comments 已把 baseline 压到 26。 | 无需特别操作。 | 已落地 |
| stable-identity rename | 未实现。foch 还不会把 `Remove(item-named-X) + Append(item-named-X-renamed)` 按 `name=` / `id=` / 等价 key 配对。 | N=37 只命中 1 条，短期 ROI 低。 | 用 `--interactive` 或 `foch.toml` 仲裁；保留 fixture 供后续 rename matcher 使用。 | F1b-rename，暂排 UI1 后 |

## 4. 跨版本漂移诊断

| 诊断 | 当前状态 | 检测时机 | 能发现什么 | 不能发现什么 | 现在可做 | 跟踪 |
|---|---|---|---|---|---|---|
| D1 dep-misuse（当前 finding id `D001`） | 已实现 | analyzer / `check` / merge report | mod 在 `descriptor.mod` 声明依赖，但语义索引里没有实际引用被依赖 mod 的符号。 | 不能证明依赖一定错误；也不能覆盖所有文本 / 运行时隐式依赖。 | 审阅依赖边；必要时用本地 override 忽略。 | D1 landed |
| V2 supported_version（当前 finding id `V001`） | 已实现 | analyzer / `check` / merge report | `descriptor.mod` 的 `supported_version` 与实际 game version 的 major/minor 不匹配。旧版本为 info，新版本为 warning。 | 不能证明具体符号 stale；只是版本漂移信号。 | 升级 mod、改 playset、或接受风险。 | V2 landed |
| V1b stale-vanilla-target | 开发中；未见随 `a37ffb8` HEAD 发布的提交 | 目标为 analyzer | mod 修改 / 引用的 vanilla target 在当前游戏版本中已漂移或消失。 | HEAD 还不能给稳定诊断。 | 手工比对 vanilla 文件；关注后续 V1b。 | V1b |
| V1a vanilla-symbol-index | 未实现 | 目标为 analyzer | 以 vanilla symbol index 判断 mod 对 base game symbol 的引用 / 覆盖是否仍有效。 | 当前无法系统性回答。 | 对关键符号做人工验证。 | V1a |

仍未覆盖的漂移：vanilla DLC dependency 引入的 in-mod scope errors、mod 与 vanilla 之间的文件编码漂移、跨语言 mod 的 `.yml` localisation key drift。

## 5. CLI / engine 配置耦合（open bug）

| 限制 | 当前状态 | 用户影响 | 现在可做 | 来源 | 跟踪 |
|---|---|---|---|---|---|
| `foch merge --config PATH` 未贯穿到 merge engine | CLI flag 可解析，但 merge engine 读取 resolution 时仍调用 `FochConfig::try_load(playset_root)`，没有使用 CLI 指定 path。 | 你以为加载了某个 `foch.toml`，实际 resolution 可能来自 playset 旁边或用户默认路径。 | 把目标 `foch.toml` 放到 playset/root 期望位置；或设置 `FOCH_CONFIG_DIR`；或临时调整 `~/.config/foch/foch.toml`。运行后检查 `.foch/foch-merge-report.json`。 | [`crates/foch-engine/src/merge/execute.rs`](./crates/foch-engine/src/merge/execute.rs) `load_resolution_map()` | config plumbing bug |

## 6. UI / UX gaps

| 限制 | 当前状态 | 用户影响 | 现在可做 | 跟踪 |
|---|---|---|---|---|
| TUI conflict resolver | 未随 HEAD 发布。UI1 alpha P0 仍待完成。 | 不能像 Irony Merge Viewer 那样在树形 UI 中逐块 copy / edit / resolve。 | 使用 `foch merge --interactive`；或手写 `foch.toml [[resolutions]]`。 | UI1 |
| VS Code merge UI | [`packages/vscode-foch`](./packages/vscode-foch) 已存在，主要是 LSP / diagnostics / completion / goto definition。未接入 merge conflict workflow。 | 可编辑与诊断脚本，但不能在 VS Code 内完成 merge 仲裁闭环。 | 用 CLI 生成报告，再在编辑器里人工查看相关文件。 | VS Code merge UI |
| GUI / desktop app | 未实现。 | 没有 collection manager、drag/drop load order、图形化 patch mod 管理。 | 继续使用 Paradox Launcher / Irony 管理 playset；用 foch 做分析与 deterministic merge。 | GUI backlog |
| 非 TTY 场景 | `--interactive` 在非 TTY 会 defer，不会卡住 CI。 | CI 中 unresolved conflict 仍需预置 resolution。 | 预写 `foch.toml`，或显式 `--fallback`。 | UI1 / CI workflow |

## 7. 与 Irony / CWTools 的对比

| 维度 | foch 当前强项 | Irony / CWTools 当前强项 | 取舍 |
|---|---|---|---|
| Merge 理论 | deterministic structural merge、AST-level diff / patch、dependency DAG level-by-level apply、EU4-specific merge policy。 | GUI merge viewer、手工 patch 工作流、成熟 collection 管理。 | foch 更适合可复现 pipeline；Irony 更适合人工操作。 |
| Schema 覆盖 | sparse schema：只为已验证 EU4 root 写 ContentFamily policy。 | dense `.cwt` schema 生态，多游戏覆盖更广，尤其 Stellaris / CWTools 经验。 | foch 故意少而准；代价是未注册 root 会保守 fallback。 |
| IDE / UX | CLI、JSON artifact、LSP / VS Code 基础功能。 | GUI polish、可视 conflict navigation、外部 merge tool 集成。 | foch 还缺 UI1 / GUI。 |
| EU4 精度 | EU4 `GameProfile`、semantic index、DAG-aware merge 是主线目标。 | Irony 的完整 conflict solver 主要强在其支持最完整的游戏生态；EU4 自动结构 merge 不是同一定位。 | EU4 power user 可以用 foch 补足 deterministic merge，但仍可能保留 Irony 管理 playset。 |

## 8. Performance posture

| 限制 | 当前状态 | 用户影响 | 现在可做 | 跟踪 |
|---|---|---|---|---|
| 冷启动成本 | N=37 mod merge 在 macOS release build 冷启动约 33s。 | 对交互式反复调 resolution 来说仍偏重。 | 使用 release build；先用 `check` / `merge-plan` 定位，再少量重跑 `merge`。 | C1-C4 / incremental work |
| 增量 merge cache | 尚无端到端 merge IR / output-path 级增量复用。README 中已有 parser cache 与 mod semantic snapshot cache，但这不等于完整 incremental merge。 | 大 playset 仍主要按 mod / 文件数量近似线性扩展；>50 mods 应预期更长等待。 | 保持 playset 小批量验证；避免在一次 run 中混入大量无关改动。 | C1（mod parse/cache line）及后续 C2-C4 |
| Base data | 基础游戏不再靠隐式扫描缓存；需要安装 / 构建 base data。 | 初次配置失败会影响诊断与 merge 质量。 | 先跑 `foch data install eu4 --game-version auto`，或用本机 EU4 构建并安装。 | data workflow hardening |

## 9. 不支持的游戏 / 功能

| 范围 | 当前状态 | 用户影响 | 现在可做 | 跟踪 |
|---|---|---|---|---|
| CK3 / HOI4 / Stellaris | 当前是 EU4-only。`GameProfile` 与 ContentFamily registry 都围绕 EU4。 | 其他游戏不能期待正确 scope、symbol、merge policy。 | 不要把 foch 当通用 Paradox merger；新增游戏需要独立 `GameProfile`、ContentFamily registry、fixtures、base-data probe。 | new game profile work |
| Binary assets | 不做纹理、音乐、模型等二进制内容的结构 merge；最多 copy-through / overlay / manual conflict。 | 两个 mod 改同一 binary asset 时，foch 不会理解内容差异。 | 用专业资源工具或手工选择；在 foch 中只保留路径级决策。 | binary asset policy |
| Localisation 深度合并 | localisation 兼容是独立 workstream，不是当前 merge core 的完成条件。 | 跨语言 key drift、编码差异、翻译覆盖关系仍可能需要人工检查。 | 用 `check` 的 localisation diagnostics 做入口；关键语言包手工抽样。 | localisation workstream |
| 完整 mod manager | 不管理订阅、启动器状态、collection export/import、成就过滤、Steam 元数据。 | 不能替代 Paradox Launcher / Irony 的管理面。 | 让现有 manager 管 playset；foch 负责分析、merge、报告、CI。 | product backlog |

## 10. 采用建议

如果你的目标是：可复现 EU4 merge、可审计 JSON artifact、CI 友好、愿意为 26 条 residual 手工写 resolution，foch alpha 已经可试用。

如果你的目标是：零配置 GUI、跨游戏 schema breadth、Stellaris-first conflict solver、非技术用户可维护 patch mod，当前应继续以 Irony / CWTools 生态为主，把 foch 作为 EU4 结构分析补充。
