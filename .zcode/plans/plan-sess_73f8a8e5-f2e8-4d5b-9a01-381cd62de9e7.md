# 动态调整贸易黑洞 EU4 Mod — 设计与原型实现计划

## Context(背景)

EU4 的贸易网络拓扑(`common/tradenodes/00_tradenodes.txt`)是开局即固定的:流向恒定,终点节点(黑洞)固定为英吉利海峡/威尼斯/热那亚。这不合理——贸易应流向有购买力的地方(税基高=富裕=购买力),而不是永远向西欧收敛。传统"贸易黑洞"mod 只能静态改写 tradenodes 文件:整个文件覆盖、与其他贸易 mod 必然冲突、且无法随局势演化。

本仓库(foch)已有完整的 Windows x64 DLL 插件框架:`foch_eu4_host.dll`(VERSION.dll 代理)在游戏入口前加载插件,通过 C ABI(`foch_plugin_get_api`)提供 `find_pattern` / `hook_create`(优先级链式 detour)/ `patch_create`、config JSON、目录、日志与 events.jsonl 状态流。基于该框架可以**在运行时改写内存中的贸易路线表**,实现真正的动态拓扑,且完全不碰 `00_tradenodes.txt`(与其他 mod 天然无合并冲突)。

用户已确认的设计决策:
1. **动态依据(经济梯度)**:生产力量高 → 更适合当上游(产出贸易值);贸易力量高 → 中转节点;**税基高(购买力)→ 下游/终点**。贸易沿财富梯度收敛,黑洞在富裕节点自然涌现(而非硬编码节点清单)。
2. **调整动作**:仅"关闭/恢复外流路线"(outgoing routes)。不新增边、不拦截转出计算;重定向通过剪枝涌现——关闭通向贫穷盆地的边后,流量自然走剩余(更富)的分支。
3. **交付**:一直做到能跑的原型(含逆向)。

## 核心机制设计

### 引擎侧事实(逆向目标,EU4 1.37.5 Steam x64)
- `common/tradenodes` 由 `CTradeNodeDatabase` 加载(仓库 `src/packages/foch/src/game/eu4/content/rules/1.37.5.json` 已记录该类名)。每个 `CTradeNode` 持有 outgoing/incoming 路线容器与缓存聚合值(当前贸易值、总贸易力量——贸易地图 UI 使用的就是它们)。
- 贸易传播按月(及交易时)沿路线图重算。将某节点 outgoing 置空 ⇒ 引擎视其为终点节点 ⇒ 贸易值全部留存(黑洞语义,与英吉利海峡一致)。

### 插件行为(月度策略引擎)
1. **只读观察**(每月):经安全读取聚合每节点 {总税基, 生产, 总贸易力量, 当前贸易值}。税基/生产通过对节点内省份的遍历聚合(CProvince 最小字段集:tax、production、所属节点、贸易力量),贸易力量优先读引擎缓存值(与 UI 一致)。
2. **策略(财富梯度剪枝)**:对每个节点 N 的每条外流边 E(目标 T):
   - 计算 T 经"当前开放边集"可达的下游盆地财富(可达终点节点的税基加权,按距离衰减);
   - **关闭** E,当 N 自身税基 ≥ 盆地财富 × 收敛系数(阈值可配,默认如 1.2)——即边通向比自己穷的地方;
   - N 的所有边都被判关闭 ⇒ N 成为黑洞(局部财富峰值);
   - **恢复** E,当不等式以滞回带(如 0.8 倍)反向成立——下游因发展/征服变富后贸易回流;
   - 滞回带 + 月度节奏 + 每月每节点最多关/开 1 条边,防止抖动。
3. **安全性约束**:只删边不改加边 ⇒ 子图仍是 DAG、无环;不重分配容器(仅改写既有条目字段/count,具体字段待 M0 逆向定夺,候选:count 递减、multiplier 置零、条目交换);所有改动在游戏线程安全点(月度 tick detour 内)完成;panic/异常严格限定在 ABI 边界内(宿主要求)。
4. **存档/读档**:路线图是否随存档快照待 M1 验证;若读档重置为 DB 状态,下月策略自动重算(可接受);若持久化,读档后先恢复原拓扑再重算。
5. **版本锁定**:init 时校验 `exe_sha256` == 已逆向的 1.37.5 Steam x64 摘要,不匹配即 `refused`(与 Menu patch 的做法一致);偏移全部经 `find_pattern` 签名解析(抗 ASLR),签名与偏移表按 exe 摘要归档。

## 项目结构(新增 workspace 成员)

```
src/packages/foch-plugin-trade-blackhole/
├── Cargo.toml          # crate-type=["cdylib"], publish=false, deps: foch-plugin-abi(路径依赖)
├── README.md           # 设计说明、逆向记录(验证日期)、打包/启动手册
├── src/
│   ├── lib.rs          # ABI 入口:foch_plugin_get_api、init(版本校验/签名解析)/status
│   ├── offsets.rs      # 按 exe 摘要组织的偏移+pattern 签名表(M0 产出)
│   ├── engine.rs       # CTradeNode/CTradeProvince 只读视图 + 路线条目安全改写
│   ├── policy.rs       # 纯 Rust 财富梯度策略引擎(无引擎依赖,单元测试全覆盖)
│   └── hook.rs         # 月度 tick detour:调用原函数后执行观察+策略
└── pkg/                # 打包模板:foch-plugin.toml + 构建产物 dll + [[files]] sha256
```

`foch-plugin.toml`(格式=1,仓库已支持自包含原生包导入,无需 registry 行/GitHub release):
```toml
schema = 1
[plugin]  id = "dev.foch.trade-blackhole" ... 
[target]  platform="windows-x86_64" game="eu4" game_versions="1.37.5" abi_major=1
[entry]   kind="native" path="trade_blackhole.dll" phase="deferred"
[config]  # enable、收敛系数、滞回比、每月最大变更边数、节点黑白名单、观察日志开关
```
配置字段声明后默认值自动进入 plan 的 config_json;用户改值目前需手改 selections.toml(框架暂无 config 编辑命令,README 说明即可)。

注意:插件包目前无法携带 EU4 脚本内容(descriptor.mod/common/),运行层也不会为插件写 dlc_load.json——本 mod 恰好 **DLL-only、零脚本文件**,不受此限制。

## 里程碑

**M0 逆向 spike(成败关键,先做)**
- 用 `src/tools/eu4-analysis`(PyGhidra,Ghidra 12.1.3 + JDK 21)分析带符号的 macOS 1.37.5 Mach-O(仓库已分析过,sha256 已在 rules JSON 记录)取得结构/函数名;再对本机 Steam Windows `eu4.exe`(PE,已剥符号)以常量/字符串/签名匹配定位同源位置,产出 `find_pattern` 签名。
- 目标结构:`CTradeNodeDatabase` 全局与实例、`CTradeNode` 布局(outgoing/incoming 容器、缓存值、节点名/索引)、`CTradeNode` 的省份归属、`CProvince` 最小字段(tax/production/trade power/节点索引)、月度 tick 或贸易重算入口函数。
- 产出:`offsets.rs` 签名表 + README 逆向记录。工具当前面向 Mach-O,需为 PE 路径写少量 PyGhidra 脚本(属预期内工作)。

**M1 最小可玩原型(硬编码黑洞,验证改写原语)**
- 插件在首次月度 tick 将单一硬编码节点(如吕贝克)outgoing 全部关闭;再下一月恢复,交替验证。
- 验证点:贸易地图路线箭头消失/恢复、该节点贸易值留存(对照 UI)、月度重算稳定、读档行为、`foch plugin status` 事件流、游戏不崩溃。

**M2 只读观察器**
- 月度 dump 全节点 {税基、生产、贸易力量、当前值} 到插件日志目录;与游戏 UI 抽查对数。数值可信后才进 M3。

**M3 策略引擎上线**
- `policy.rs` 财富梯度剪枝 + 滞回;config 全字段生效;观察日志记录每次关/开决策(节点、边、两侧财富、判定值)。
- 纯函数单元测试:合成贸易图上的收敛、回流、无环、防抖行为。

**M4 打包与验收**
- `foch plugin import ./pkg` → `enable` → `plan` → `launch` 全链路;README 记录本机验证结果与日期(仿 foch-eu4-host README 风格)。
- 可选:`foch_dev` 增加 plugin-package 子命令(算 sha256、组装 pkg/),先手动命令亦可。

## 验证

- 每个 M:工作区 `cargo fmt --all --check`、`clippy -D warnings`、`cargo test -p foch-plugin-trade-blackhole`;policy 纯逻辑全覆盖。
- ABI 合规:复用 foch-eu4-host 的测试思路——插件 DLL 在宿主 fixture 环境下初始化、对错误 exe 摘要报 refused。
- 端到端(真实游戏):`foch plugin launch --playset default` + `foch plugin status --run-dir ...`,按 M1/M2/M3 清单人工核对;events.jsonl 决策日志为准。
- 诚实边界:框架不证明可玩性;联测(现有 `#test` 事件测试框架目前只走自身 runner,不加载插件宿主)列为后续扩展,不阻塞本原型。

## 风险与对策
- **M0 逆向不达**(路线表布局不明)→ 降级方案:改用 Q2 备选"hook 转出计算乘系数"(不动结构);M0 结束时据实决策并汇报。
- **改写原语不稳定**(count 改写引发越界)→ 候选原语逐一试,配上 config 一键全恢复 + 立即禁用路径。
- **贸易地图 UI 缓存**不刷新 → M1 验证;若缓存,寻 UI 重算触发点(逆向追加项)。
- **性能**:月度 ~60 节点 BFS + 聚合,微秒级,无风险;hook 内严格快速返回(宿主要求)。
- 铁人/成就因 DLL 注入而不可用,Steam 1.37.5 x64 之外版本一律 refused——README 明示。
