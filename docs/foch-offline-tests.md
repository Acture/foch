# EU4 离线测试层:反编译证据钉扎的模拟

状态:切片五已实现,2026-11-15。新入能力包(全部 CWT 核实名与形状):**战争与同盟**(`declare_war_with_cb`/`white_peace`/`create_alliance`/`break_alliance` + `offensive_war_with`/`defensive_war_with`/`is_at_war`/`alliance_with`;注意 EU4 没有 `war_with` 触发器,CWT 已纠正);**枚举槽位**(`change_religion`/`change_primary_culture`/`change_government`/`set_government_rank` + 对应触发器,未写先读拒绝);**`every_country`**(迭代"模型世界"——用例点名的国家集合,非全游戏国家集,文档与结果注记声明);**决议目标**(`tests/` 块 `take_decision = <名>`:potential/allow 不成立→拒绝并说明,成立则执行 effect 块;在线编译干净拒绝"run with --offline")。**数值国家槽位已提供**(mana 三系、国库、稳定、威望、正统/共和传统/虔信/部族统一/专制、陆海传统、人力,共 14 对 add_*/触发器):模型初值统一为 0(记录在清单 facts 与每条轨迹里),加法累加,触发器为 ≥ 比较,失败信息给出模型值。**初值是否与游戏开局一致由作者判断**——这是作者反馈层,不是游戏证据;base-snapshot 播种落地后初值会更接近真实开局,在那之前差分校准不覆盖数值初值。相对切片三的变更:**观察者模式默认开启**(`--no-observe` 关闭);**`ai = on` 不再被离线层拒绝**——模型只执行作者脚本,AI 是否行动是运行器关注点、改变不了作者语句的效果,故降为结果注记(在线层默认 `ai = off` 的确定性设计不变)。**首个真实反编译钉扎批已在 Windows 本机跑通**(eu4.exe 1.37.5 + Ghidra 12.1.3 + JDK 21):基础设施全部工作(PE 解析、`.pdata` 边界、RIP 相对 `lea` 字符串引用、批量容错、产物落盘),16 个原语全部如实记为 `missing`——真实二进制的注册形状(疑为静态表初始化而非直接调用参数)需要下一轮按证据迭代定位策略;产物存于 `target/eu4-analysis/semantics-pinned-real-run.json`,清单保持 `unpinned`。新入:scripted_trigger/effect 内联(`common/scripted_*` 收集,效果/条件块中按名内联,类型错配与互内联环拒绝);`random`/`random_list` 的确定性特例(chance 0/100、唯一存活分支),其余随机拒绝;观察者模式(`--observe`:记录并打印步骤边界、事件触发/调度/投递、旗标与变量写入的建模轨迹)。**hidden 事件 option 语义已真机验证并修正**:EU4 1.37.5(原生 Windows,2026-11-05)实测触发 hidden 事件不执行 option 内容,error.log 为空;模型与该证据一致(差分:离线与真机同因 FAIL 同一子句)。解释器与结果类型在 `src/packages/foch-test/src/interpret`,CLI 为 `foch test --offline`,提取器为 `python -m eu4_analysis semantics`——支持两条真实提取路径:**Windows PE eu4.exe(本机即可跑,`.pdata` 精确函数边界 + 关键字字符串引用定位,无需符号)**与 macOS 符号版 Mach-O(符号模式定位);两者都需要 Ghidra 12.1.3。已建模:旗标、变量、`if` 控制流、国家作用域切换与 THIS/ROOT/PREV/FROM 别名、`country_event` 事件链(含 `days = N` 延迟投递)。差分校准两组:runtime-tests 夹具(与 2026-10-02 录制真实游戏证据一致)与在线 pipeline 语义语料。尚未完成:首个反编译钉扎批(当前清单 `unpinned`)、数值槽位建模。

## 设计前提:离线层解决什么

在线框架(见[使用说明](foch-runtime-tests.md))每次运行要启动真实 EU4,单例约 60 秒,且必须有游戏安装。作者反馈回路需要毫秒级、无需游戏/Steam/base data 的执行。仓库既有研究结论是"单独游戏 DLL 模拟不可行"与"不重写游戏机制模拟器(作为验证替代)"——离线层通过两条原则与该结论共存:

1. **分层**:静态检查(收集、lint)→ 离线解释器(新)→ 真实游戏(唯一权威判定)。离线层类比 rustc 类型检查与 Miri 之于真实执行:它给作者快速反馈,不证明游戏行为。
2. **反编译钉扎**:解释器执行的每个原语都是 [`semantics.json`](../src/packages/foch-test/src/interpret/semantics.json) 里的一条记录,其语义以 eu4.exe 中反编译出的处理函数为依据;证据缺失即拒绝,绝不近似。

| 约束 | 事实 | 对设计的影响 |
| --- | --- | --- |
| 离线没有开局状态 | 国家数值(mana/国库/稳定/威望)的开局值在 base snapshot 里 | 数值槽位读写一律拒绝(O3 从基础快照播种);flag 与变量由脚本绝对赋值,可知 |
| 模拟不可替代游戏 | 既有结论 | 结果是独立类型(`Interpretation`),JSON 顶层 `layer: "offline"`,永不进入 `Status`/`Report`/`cargo acceptance` |
| 语义必须可审计 | wiki 不可靠、CWT 只是线索 | 每个原语的 facts(如 `check_variable` 的 `comparison = ">="`)记录在清单中,由单测与反编译伪代码双重约束 |
| 国家集合未知 | 没有 base snapshot 就不知道世界上有哪些国家 | `every_country` 等"所有国家"迭代一律拒绝;只跟随作者显式写出的标签作用域 |
| 随机不可复现 | EU4 的 RNG 算法与种子行为未经本项目验证 | `random`/`random_list` 一律拒绝,不用自造随机数冒充 |

## 语义钉扎清单

`src/packages/foch-test/src/interpret/semantics.json` 是唯一事实源,被 `include_str!` 内嵌进二进制,同时是提取器的输入:

```json
{
	"format": 1,
	"status": "unpinned",
	"primitives": {
		"check_variable": {
			"kind": "trigger",
			"facts": { "comparison": ">=", "operands": "which, value" },
			"handler": null
		}
	}
}
```

- `status: unpinned` —— 尚无反编译批次。模型照常解释,但每个结果携带 `evidence: "unpinned"`,CLI 输出首行声明;这是过渡态,不是常态。
- `status: pinned` —— 提取批已回填 `handler`(符号、地址区间、伪代码 SHA-256)。此时**无钉的原语一律拒绝**,理由为 "no decompiled handler evidence"。

提取命令(输出为候选文件,人审后替换内嵌清单,流程同 builtin catalog):

```text
# Windows:本机 eu4.exe 即可,无需符号
uv run --locked --project src/tools/eu4-analysis python -m eu4_analysis semantics \
  --binary "C:/.../Europa Universalis IV/eu4.exe" --game-version 1.37.5 \
  --catalog src/packages/foch-test/src/interpret/semantics.json \
  --output target/eu4-analysis/semantics-pinned.json
```

两条定位路径,由二进制格式自动决定:

- **Windows PE(无符号)**:x64 PE 的 `.pdata`(RUNTIME_FUNCTION 表)提供与 Mach-O LC_FUNCTION_STARTS 等价的**精确函数边界**;注册函数通过"关键字字符串 → `.text` 中 RIP 相对 `lea` 引用扫描 → 所在 `.pdata` 函数"定位(`eu4_analysis/pe.py`)。
- **macOS Mach-O(带符号)**:按 `--effect-registry`/`--trigger-registry` 符号模式(默认 `*CEffectDatabase*` 等,可用参数覆盖)找到注册函数。

两条路径最终都通过 `x86.trace` 追踪注册调用中与关键字字符串同传的函数地址,反编译该处理函数并回填钉。找不到的原语记入 `missing`,清单保持 `unpinned`——宁可钉不满,不伪造。

一致性由单测双向保证:代码实现集必须等于清单原语集(名称与类别);`check_variable` 的实现必须与清单 `facts.comparison` 一致;PE 解析、函数边界与字符串引用定位有合成二进制单测(`tests/test_pe.py`)。

## 建模原语

全部在用例的国家 scope 中执行:

| 类别 | 原语 | 建模语义 |
| --- | --- | --- |
| 效果 | `set/clr_country_flag`、`set/clr_global_flag` | 集合的插入/移除 |
| 效果 | `set_variable`、`change_variable` | `which/value` 绝对赋值与 `+=`(未设变量按 0 起) |
| 效果 | `country_event = { id = ... days = N }` | 触发已收集的 hidden 事件:`days = 0`/缺省为立即,`days ≥ 1` 进入延迟队列 |
| 效果 | `if/else_if/else`(带 `limit`)、`hidden_effect`、`tooltip` | 控制流与透传 |
| 效果 | `log` | 空操作 |
| 效果/触发 | `TAG = { ... }`、`THIS/ROOT/PREV/FROM = { ... }` | 作用域切换:进入目标国家块,`PREV` 指向被离开的国家 |
| 触发 | `has_country_flag`、`has_global_flag`、`has_variable` | 存在性 |
| 触发 | `check_variable` | `which >= value` |
| 触发 | `always`、`tag`、`AND/OR/NOT/NOR/NAND`、`hidden_trigger` | 常量、scope 标签、逻辑组合 |

作用域链:`ROOT` 是用例的开局国家;`THIS` 是当前作用域;`PREV` 是紧邻的外层作用域(顶层不可用,预检拒绝);`FROM` 是触发当前事件的国家(仅在被触发事件内可解析)。`NOT`/`AND` 永远是逻辑关键字;`NOR` 在效果块里是国家(挪威)、在条件块里是逻辑,与 lint 足迹的取舍一致。

事件链:被触发事件的 `immediate` 在触发作用域上执行并带 `FROM`;`immediate` 内再触发形成链,immediate 环(自触发)拒绝。`country_event ... days = N` 的事件进入按日期排序的延迟队列,`advance_days` 推进时按投递日、同日按入队顺序执行——与在线层一致,同日内顺序不承诺;总投递次数上限 4096,自调度事件对会被拒绝而不是死循环。

**拒绝而非近似**(预检一次给出全部原因,带源码位置):

- 三类 key:不在清单 / 游戏已知但未建模(内置目录可辨时点名)/ 完全未知;
- `every_country` 等"所有国家"迭代——没有 base snapshot 就不知道国家集合;
- `random`/`random_list`——EU4 的 RNG 行为未验证,不自造随机;
- 国家数值槽位的读写(`add_adm_power`、`adm_power = 50` 等);
- `ai = on`(模型没有 AI 决策);
- fire 目标事件含非 `name` 的 option 内容、`after`、`trigger` 块——hidden 事件 option 执行语义未经本项目真实验证。

`advance_days` 是纯日期推进。已建模读取(flag、变量)都不随日历衰减,因此可判;引入数值槽位后需同时引入 tick 敏感规则(推进后读 tick 敏感槽位即拒绝)。

## 状态与判定

| 状态 | 含义 | 计入成功 |
| --- | --- | --- |
| `pass` | 所有 expect 子句在模型中成立 | 是 |
| `smoke` | 无 expect,步骤在模型中走完 | 是 |
| `fail` | 有子句不成立;报告给出建模观测值与子句源码位置 | 否 |
| `setup_error` | fixture 前置检查不成立 | 否 |
| `unsupported` | 用例被拒绝,附全部原因 | 不计失败;整轮全部 unsupported 时退出码为 1 |

- 检查标签与在线层同名(`expect.{step}.{clause}`、`fixture.{name}.{clause}`),失败报告可指向同一处源码。
- **xfail 不参与离线判定**:它表达的是与*游戏*行为的预期偏差,模型不是游戏;离线只报原始判定并加注记。
- skip/ignore 在选择层照常生效,进入 `not_run`。
- 失败信息只陈述模型状态("the model has no country flag X"、"the model has progress = 3, expected at least 4"),绝不冒充游戏数值——这是在线层给不了、离线层独有的定位信息。

## 命令

```text
foch test ./my-mod --offline
foch test ./my-mod --offline -k "reform and not slow" --format json
foch test ./my-mod --offline --out ./offline-results
```

不启动游戏、不需要 Steam 或 base data,也不初始化 Foch 全局配置;收集期诊断与在线层相同地阻断(可用 `--run-anyway` 覆盖模型性发现)。`--offline` 与 `--collect-only`/`--no-run`/`--game-path`/`--eu4-user-dir`/`--isolate` 互斥。输出目录写 `result.json`(顶层 `layer: "offline"` 与 `evidence` 状态)与 `junit.xml`(未运行的用例记为 `<skipped>`)。

离线结果**永不**作为合并质量或游戏行为的验收证据,也不改变 `foch test` 默认(在线)行为。

## 校准与证据纪律

- 差分组一:`src/packages/foch-test/tests/interpret.rs` 断言 runtime-tests 夹具(smoke、即时断言、延迟断言、故意失败)的离线判定与 2026-10-02 录制的真实游戏运行一致。
- 差分组二:同文件对在线 pipeline 测试所用的语料(内联注解 + fixtures + 多步骤 `tests/` 块)断言离线判定与在线断言语义一致。
- 扩语料时先加差分断言,再扩大建模集;离线与在线判定不一致是缺陷,优先修模型或收紧拒绝面。
- 本层结果不进入 `cargo acceptance` 的分母或门槛。

## 阶段计划

| 阶段 | 内容 | 需要的验证 |
| --- | --- | --- |
| O1(完成) | 解释器、钉扎清单与消费、CLI `--offline`、差分种子 | 已验收 |
| O2(完成) | 作用域切换与别名、事件链与延迟队列、junit、Windows PE 提取路径、第二差分组 | 已验收;真实 eu4.exe 提取待有游戏安装的机器执行 |
| O3 | 首个反编译钉扎批(Windows PE 或 macOS);数值槽位从 base snapshot 播种;tick 敏感规则;校准语料与偏差报告(离线 vs 录制在线结果对比)、LSP 就地离线反馈、`--offline-first` 联动 | 提取批人审;差分语料扩至含数值断言 |

## 待定问题

1. 解释器最终归属:留在 `foch-test`(测试语义)还是迁往 `game/eu4`(若合并分析或 LSP 需要复用建模语义)。
1. **多插件各自独立会话测试**:每个原生插件(foch-eu4-host 体系)一个隔离会话的启动验证——逐插件用 `foch plugin launch` 的运行层单独起局,收集 `foch-host/events.jsonl` 判定 `active`,互不共享会话;与 mod 事件测试是两条判定路径(插件行为不是 Clausewitz 脚本)。
2. hidden 事件 option 是否全部执行:需一次真实游戏实验;验证前 option 内容保持拒绝。
3. 钉扎批的更新节奏:随游戏版本重跑,伪代码摘要变化是否自动使清单失效。
4. `missing` 原语的长期存在形态:保持 `unpinned` 整体降级,还是允许按原语部分钉扎。
