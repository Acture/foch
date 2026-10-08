# EU4 测试框架：借鉴 pytest 与 Rust 的 API 设计

状态：部分实施，2026-10-09。注解机制在 `foch-annotation`，测试库在 `foch-test`，内置运行器在 `foch-runner`，CLI 与 `foch-lsp` 已接入，主库旧模块已删除；当前用法见 [使用说明](foch-runtime-tests.md)。内置运行器、关闭 AI、按条件拆分的检查已在原生 Windows 真实游戏端到端验证；真实目录保护靠备份+按内容校验/恢复。未完成：游戏完全不碰真实目录（文件 API 层方案，见私有 notes）；`tests/` 目录尚未得到 EU4 不加载的实测与 Foch 本体（合并、检查）的特殊处理，CLI 因此不收集它；共用一局、参数化、`--last-failed`、读取项目信息的静态检查、托管 CI（Linux + Wine）。基于本地工作树已有的首版事件测试（[使用说明](foch-runtime-tests.md)）和拆分前的设计记录（已归入私有 notes）。产品要求以 [作者工具链设计](foch-authoring-design.md) 为准；本文回答其中"继续设计的事项"里的多步骤、准备内容组织、参数化、失败定位和工作流部分。

## 设计前提：EU4 与 pytest/Rust 的根本差异

pytest 和 libtest 的很多设计默认"单个用例便宜、状态可重置"。EU4 正好相反，借鉴时必须先承认这些约束：

| 约束 | 实测或已知事实 | 对设计的影响 |
| --- | --- | --- |
| 单例成本高 | 本地实测每例启动到清理约 59–64 秒 | 不在运行期才发现错误：能静态查的都在收集期查；失败信息一次给全 |
| 全局状态不可重置 | 没有已验证的"回滚到开局"手段 | 默认每例独立进程；共享会话必须作者显式选择且证明无污染 |
| 断言只能得到布尔值 | 生成事件只能 `log` 出 PASS/FAIL | 失败定位靠拆分子句，不伪造实际数值 |
| 源文件必须仍是合法 EU4 mod | 注解只能存在于注释中 | 同文件测试只用 `#` 注解；复杂测试放到 EU4 不加载的目录 |
| 时序粗糙 | 延迟事件不能保证与同日其他事件的先后顺序 | 时间推进以天为单位，报告不承诺同日内的顺序 |

因此借鉴的是 **作者体验与工作流**（发现、命名、fixture、参数化、标记、筛选、报告），而不是 pytest 的运行模型。

## 概念映射总览

| pytest / Rust | Foch EU4 对应 | 阶段 |
| --- | --- | --- |
| `#[test]`、`def test_*` | `#test(...)` 注解（已实现） | 已有 |
| `#[cfg(test)] mod tests`（同文件单元测试） | 事件上方的同文件 `#test` 注解 | 已有 |
| Rust `tests/` 集成测试目录、pytest `tests/` | mod 根目录 `tests/`：多步骤测试、fixture、辅助事件，EU4 不加载 | P2 |
| pytest node id `file::Class::test[param]` | `events/reforms.txt::reforms.1::name[SWE]` | P1 |
| `-k EXPR`、Rust 测试名过滤 | `-k`（已实现）+ node id 精确选择 | P1 |
| `@pytest.mark.skip` / `#[ignore]` | `#skip(reason=...)` / `#ignore(reason=...)` | P1 |
| `@pytest.mark.xfail(strict=True)` | `#xfail(reason=..., strict=yes)` | P1 |
| 自定义 mark + `-m` | `#mark(slow, war)` + `-m "war and not slow"` | P1 |
| pytest 断言内省、`assert_eq!` 输出 | `expect` 按顶层子句拆分，失败指向具体行 | P1 |
| Rust 编译期类型检查 | 收集期按 CWT 校验 effect/trigger 上下文、tag、事件引用 | P1 |
| `--lf`、`-x`、`--maxfail` | `--last-failed`、`--fail-fast`、`--max-fail N` | P1 |
| `--junitxml`、libtest JSON | `--format json\|junit` | P1 |
| fixture、`conftest.py`、fixture 依赖 | `tests/` 中的 `fixture = {...}`，`use = {...}`，可互相依赖 | P2 |
| fixture setup error 与 test failure 区分 | fixture 的 `check` 不成立 → `setup_error`，不是 `fail` | P2 |
| `@pytest.mark.parametrize` | `#parametrize(tag=[SWE, DAN])`，笛卡尔积，展开为独立用例 | P3 |
| pytest-xdist `-n` | `--jobs N` 并行游戏实例，受资源预算约束 | P3 |
| fixture `scope="session"` | `isolation = shared`，作者显式选择，前提是证明无污染 | 研究 |
| proptest / quickcheck、shrinking | `#fuzz(...)`，带预算，在独立起点重放后缩减 | P4 |
| yield fixture 的 teardown | **不提供**：每例独立进程，结束即丢弃状态 | — |
| `#[should_panic]` | **不直接提供**：EU4 没有 panic；"预期失败"由 `xfail` 表达 | — |

## 一、用例身份：node id

借鉴 pytest，每个用例有一个人可读、可复制、可传回 CLI 的身份：

```text
events/reforms.txt::reforms.1::reform_grants_flag
events/reforms.txt::reforms.1::reform_grants_flag[tag=DAN]
tests/war.txt::declare_then_peace
```

- 结构为 `源文件相对路径::目标::用例名[参数]`。同文件注解的目标是事件 ID；`tests/` 中的独立测试没有附着目标，省略该段。
- 未写 `name` 时用注解在该事件上的序号 `#0`、`#1`，并在收集时给出提示：序号会随注解增删变化，不适合长期引用。
- node id 是展示与选择用的身份。现有基于用例内容哈希的稳定身份和每次运行的 `run_id` 保持不变；报告同时记录两者。

CLI 选择沿用 pytest 习惯：

```text
foch test ./my-mod                                   # 全部
foch test ./my-mod/events/reforms.txt                # 一个文件
foch test "./my-mod/events/reforms.txt::reforms.1"   # 一个事件上的全部用例
foch test ./my-mod -k "reform and not slow"          # 名称表达式
foch test ./my-mod -m war                            # 按标记
foch test ./my-mod --last-failed                     # 只跑上次失败
```

## 二、同文件注解（内联单元测试）

保持现有 `#test(...)` 语法与语义不变，新增若干**独立注解**，叠放在同一事件上方，作用于下方所有 `#test`，形式类似 Rust 属性和 pytest 装饰器：

```text
namespace = reforms

#mark(reform)
#xfail(reason="依赖 1.38 修改的 government_reform 行为", strict=yes)
#test(time=1444.11.11, tag=SWE, name=grants_flag,
#     effect={ clr_country_flag = reform_done },
#     expect={
#         has_country_flag = reform_done
#         adm_power = 50
#     })
#skip(reason="等待准备事件")
#test(time=1444.11.11, tag=SWE, name=needs_war, expect={ is_at_war = yes })
country_event = {
	id = reforms.1
	...
}
```

规则：

- 修饰注解作用于**紧随其后的那一个** `#test`（像 Rust 属性作用于下一个 item）。需要作用于整个事件时，放在第一个 `#test` 之前并写 `scope=event`；不提供隐式的全局继承，避免作者看不出一个用例被谁修饰。
- 未知注解名是收集错误，不静默忽略（避免 `#tset(` 之类拼写让测试消失）。
- 所有注解行仍是 EU4 注释，文件可直接被游戏加载，这一兼容要求不变。

## 三、标记与结果状态

| 注解 | 行为 | 报告状态 |
| --- | --- | --- |
| `#skip(reason=...)` | 不编译不运行 | `skipped` |
| `#ignore(reason=...)` | 默认不运行；`--include-ignored` 运行，`--ignored` 只运行这些 | 运行时按普通用例，否则 `ignored` |
| `#xfail(reason=..., strict=yes/no)` | 预期断言失败 | 失败 → `xfailed`（通过）；通过 → `xpassed`，`strict=yes` 时算失败 |
| `#mark(a, b)` | 只用于 `-m` 筛选 | 不影响状态 |

`xfail` 只吞掉**断言失败**。运行错误、超时、缺日志、非空 `error.log`、协议错误永远不能被 `xfail` 变成通过——这和 pytest 中 `xfail` 不掩盖收集错误一致，也满足作者工具链设计里"未完成运行都不能算通过"的要求。

完整状态集合（在现有 `pass/fail/incomplete/error/runtime_error` 和 `smoke` 基础上扩展）：

| 状态 | 含义 | 计入成功 |
| --- | --- | --- |
| `passed` | 所有 expect 子句成立 | 是 |
| `smoke` | 无 expect，调用链完整 | 是，但单独计数，不显示为"行为正确" |
| `xfailed` | 标记的预期失败如约失败 | 是 |
| `skipped` / `ignored` | 未运行 | 不计失败；`--strict-skips` 可让其失败 |
| `failed` | 有 expect 子句不成立 | 否 |
| `xpassed` | 预期失败却通过 | `strict=yes` 时否 |
| `setup_error` | fixture 前置检查不成立（P2） | 否 |
| `incomplete` / `error` | 检查未到齐 / 协议违规 | 否 |
| `runtime_error` | 启动、崩溃、超时、缺日志、`error.log` 非空 | 否 |

## 四、断言内省：按子句拆分 expect

pytest 的价值在于失败时直接看到哪一处不成立。EU4 只能得到布尔值，但 `expect` 块的顶层子句之间本来就是 AND 关系，且 trigger 没有副作用，因此可以在**同一 scope、同一时刻**分别求值，结果与整体求值等价：

```text
#test(time=1444.11.11, tag=SWE, name=grants_flag,
#     expect={
#         has_country_flag = reform_done      # expect[0]
#         adm_power = 50                      # expect[1]
#         OR = { is_emperor = yes  is_elector = yes }   # expect[2]
#     })
```

生成的检查事件对每个顶层子句输出 `PASS expect[i]` / `FAIL expect[i]`，判定器把编号映射回注解中的原始字节范围。终端输出：

```text
FAILED events/reforms.txt::reforms.1::grants_flag  (62s)
  events/reforms.txt:7:7  expect[1] 不成立
  |     adm_power = 50
  |     ^^^^^^^^^^^^^^
  其余 2 个子句成立。只记录布尔结果，未读取实际数值。
```

- 只拆顶层。`OR`、`NOT`、`AND` 等嵌套块作为一个整体报告，不深入，因为拆开会改变语义。
- 这是协议的检查列表变化：现有 `checks = ["scope", "invoked", "expect"]` 变为 `expect[0..n]`。按 crate 设计文档的约定，这属于运行协议的语义变化，需要提升协议版本，而不是悄悄改 v1。
- 读取实际数值（相当于 `assert_eq!` 的 left/right）**本阶段不承诺**。若后续验证 EU4 `log` 中的本地化命令能稳定输出变量值，再增加显式的 `observe = { ... }` 参数，并在报告中注明数值来源。

## 五、收集期静态检查（Rust 的编译期保证）

每次运行成本约一分钟，所以要像 Rust 编译器一样，把能在运行前发现的问题都在 `foch test --collect-only` 和 LSP 中报告：

| 检查 | 依据 | 失败时 |
| --- | --- | --- |
| `effect` 中只能有 effect，`expect` 中只能有 trigger | 已嵌入的 CWT 规则包 | 收集错误，指向原位置 |
| 子句在 country scope 下合法 | CWT scope 信息 | 收集错误 |
| `tag` 在被测 mod + base data 中存在 | 项目索引 | 无 base data 时降级为警告，并注明未检查 |
| `fire` / `use` 引用的事件和 fixture 存在 | 项目索引 | 收集错误 |
| fixture 依赖无环 | 收集到的 fixture 图 | 收集错误，列出环 |
| 参数化展开后的用例数不超过预算 | `--max-cases` / 配置 | 收集错误 |

规则包中存在某个 trigger，不等于运行时支持它；静态检查通过不代表测试会通过，只排除"白跑一分钟"的错误。无 base data 时收集与编译仍可工作（现有要求），只是相应检查降级并明确说明。

## 六、`tests/` 目录：集成测试与 fixture（P2）

借鉴 Rust 的 `tests/` 和 pytest 的 `conftest.py`：mod 根目录下的 `tests/` 存放多步骤测试、fixture 和测试专用辅助事件。

```text
my-mod/
├── events/reforms.txt          # 生产内容，可带 #test 注解
└── tests/
    ├── fixtures.txt            # fixture 定义
    ├── war.txt                 # 多步骤测试
    └── events/helpers.txt      # 测试专用辅助事件，只编译进测试层
```

**前置验证**：设计依赖"EU4 不加载 mod 根目录下的 `tests/`"。这需要按现有研究方法在真实游戏中验证（放入会报错的内容，确认 `error.log` 为空）后才能定稿。未验证前不实现。

由于 `tests/` 不被游戏加载，这里可以使用**非注释**的 Clausewitz 块，作者获得完整的编辑器补全和语法高亮；`tests/events/` 中的辅助事件只进入生成的测试层，满足"纯测试辅助内容不进入生产加载"的要求。

### fixture

```text
# tests/fixtures.txt
fixture = {
	name = at_war_with_denmark
	use = { swedish_regency }            # fixture 可依赖 fixture，按依赖拓扑执行
	effect = {
		declare_war_with_cb = { who = DAN casus_belli = cb_restore_personal_union }
	}
	check = { war_with = DAN }           # 前置条件；不成立 → setup_error，而非 fail
}

fixture = {
	name = swedish_regency
	effect = { country_event = { id = my_mod_tests.1 } }   # 可调用 tests/events 中的辅助事件
}
```

- fixture 是**命名的、可复用的准备步骤**，在 `tag` 指定的国家 scope 中执行。不提供 teardown：每例独立进程，结束即丢弃状态。
- 同一用例中被多次依赖的 fixture 只执行一次（pytest 的同 scope 缓存语义）。
- 内联注解可以直接使用 fixture：`#test(time=1444.11.11, tag=SWE, use={ at_war_with_denmark }, expect={...})`。不使用 fixture 时体验与现在完全一样，保持"无需先理解具名 fixture"的产品要求。
- `check` 区分"准备失败"和"被测行为错误"，对应 pytest 中 fixture 出错报 `ERROR` 而非 `FAILED`。

### 多步骤测试

Clausewitz 块中的语句保持顺序，因此一个测试块里的有序语句就是步骤序列，读起来像一个测试函数体：

```text
# tests/war.txt
test = {
	name = declare_then_white_peace
	time = 1444.11.11
	tag = SWE
	use = { at_war_with_denmark }
	mark = { war slow }

	fire = reforms.1                     # 主动调用事件
	expect = { has_country_flag = reform_done }
	advance_days = 30
	effect = { white_peace = DAN }
	advance_days = 1
	expect = { NOT = { war_with = DAN } }
}
```

| 步骤 | 语义 |
| --- | --- |
| `effect = {...}` | 在当前国家执行原生 effects |
| `fire = <event_id>` | 调用事件；收集期要求目标支持主动调用（当前仅 hidden、is_triggered_only 的 country_event） |
| `advance_days = N` | 推进真实游戏天数，所有 `advance_days` 之和受现有上限约束 |
| `expect = {...}` | 检查点；每个检查点按子句拆分，报告为 `step[3].expect[0]` |

内联注解是这个模型的语法糖：`#test(time, tag, effect=E, advance_days=N, expect=X)` 等价于 `effect=E → fire=<下方事件> → advance_days=N → expect=X`。编译器只实现一种步骤模型，两种写法共用编译、判定和报告路径。

第一个失败的检查点之后，后续步骤仍然执行（游戏不会停），但报告只把第一个失败作为主要原因，后面的结果标为"在失败之后观察到"，避免连锁失败误导作者。

## 七、参数化（P3）

```text
#parametrize(tag=[SWE, DAN, NOR])
#parametrize(time=[1444.11.11, 1500.1.1])
#test(name=union_flag, expect={ has_country_flag = union_ready })
```

- 多个 `#parametrize` 取笛卡尔积，展开成独立用例：`...::union_flag[tag=DAN,time=1500.1.1]`。展开在收集期完成，报告和 `--collect-only` 中每个实例都可见、可单独选择。
- 被参数化的参数可以省略在 `#test` 中；两处同时给出同一参数是收集错误。
- 展开后每个日期都必须是 runner 支持的开局；不支持的组合在运行前报 `runtime_error` 的配置错误，不能静默跳过。现有本地 runner 只验证过 `1444.11.11`。
- 展开数量受 `--max-cases` 预算约束，避免一次组合出上百个一分钟的用例。
- `tests/` 中的测试块写作 `parametrize = { tag = { SWE DAN NOR } }`。

## 八、执行、并行与共享会话

默认：**一个用例 = 一个独立游戏进程**，与现有 runner 协议 v1 相同。

- `--jobs N`（P3）：同时运行 N 个隔离实例，每个有独立 profile 与输出目录，相当于 pytest-xdist。上限由 runner 声明的资源能力决定，CLI 不猜测机器能跑几个 EU4。
- `--fail-fast` / `--max-fail N`：达到失败数后不再启动新用例，已在运行的用例正常收尾并报告，不强杀成"未知"。
- 共享会话（类似 fixture `scope="session"`）**只作为研究项**：多个用例在同一局里运行可以省去启动成本，但作者工具链设计要求"任何批量运行优化都必须先证明用例之间没有状态污染"。在找到经验证的状态回滚方式（例如存档后重载）之前不实现，也不提供"同一局不同国家即视为隔离"的捷径。

## 九、报告与工作流

终端输出采用 pytest 的紧凑风格，最后给出汇总：

```text
collected 9 cases (1 skipped, 1 ignored) in 0.4s

events/reforms.txt::reforms.1::grants_flag ............ PASSED   61s
events/reforms.txt::reforms.1::smoke .................. SMOKE    59s
events/reforms.txt::reforms.1::needs_war .............. SKIPPED  等待准备事件
tests/war.txt::declare_then_white_peace ............... FAILED   64s
  tests/war.txt:14:13  step[5].expect[0] 不成立

======= 1 failed, 1 passed, 1 smoke, 1 skipped in 3m04s =======
产物：./test-results    重跑失败：foch test ./my-mod --last-failed
```

| 选项 | 借鉴自 | 说明 |
| --- | --- | --- |
| `--collect-only` | pytest | 已有；新增显示 node id、标记和参数展开 |
| `-k`, `-m` | pytest | `-k` 已有；`-m` 为标记表达式 |
| `--last-failed` | pytest `--lf` | 从上一次 `result.json` 读取失败的 node id；用例内容变化后按稳定身份匹配不到时明确提示 |
| `--fail-fast`, `--max-fail N` | pytest `-x` / `--maxfail` | 见上一节 |
| `--include-ignored`, `--ignored` | Rust libtest | 见第三节 |
| `--format text\|json\|junit` | pytest `--junitxml`、libtest `--format json` | junit 供托管 CI 展示；json 即现有 `result.json` 结构 |
| `--no-capture` | Rust `--nocapture` | 实时转发 runner stdout/stderr，便于调试环境 |
| `--keep-artifacts` | — | 默认保留产物的现有行为不变；此项留给将来的自动清理 |

可选的项目级默认值（相当于 `pytest.ini` / `conftest.py`），放在 `foch.toml`，命令行参数优先：

```toml
[test]
runner = ["python", "tools/eu4-runner.py"]
timeout = 180
default_time = "1444.11.11"   # 内联注解可省略 time
default_tag = "SWE"           # 内联注解可省略 tag
max_cases = 50
```

这是新的配置字段，需要随 `foch.toml` 文档一并定义；作者在注解里写全参数时，这一节完全不需要。

## 十、Rust 库接口

接口已实现于 `src/packages/foch-test`，以代码为准。库只做计算，不读文件、不启动进程、不打印输出：

```text
SourceFile ─collect→ Collection ─expand→ Expanded ─lint→ LintResult
                                             └──────group──→ Plan
Plan ─compile→ Bundle ─(CLI 运行 runner)→ RunArtifacts ─judge→ CaseResult
CaseResult* → Report
```

| 阶段 | 函数 | 要点 |
| --- | --- | --- |
| 收集 | `collect(&[SourceFile])` | 内联注解与 `tests/` 块；错误进 `diagnostics`，不中断 |
| 展开 | `expand(&Collection, &ExpandOptions)` | 选择（node id、`-k`、`-m`、`only`）、skip/ignore、fixture 拓扑排序与环检测、用例预算 |
| 检查 | `lint(&Expanded, &ProjectFacts)` | 调用方提供的项目事实（缺失即跳过该检查）；输出诊断与每例保守 footprint |
| 分组 | `group(Expanded, &LintResult, &GroupOptions)` | 按声明（`session=auto/alone`、项目默认、`--isolate`）与条件分组，记录 `AloneReason` |
| 编译 | `compile(&Plan, SessionId, &RunContext)` | 以一局为单位生成测试层；协议 v2：多用例、逐子句、fixture 检查、多步骤 |
| 能力 | `check_capabilities(&Bundle, &RunnerCapabilities)` | 开局日期、关闭 AI、共用一局；不支持即拒绝 |
| 判定 | `judge(&Bundle, &Plan, &RunArtifacts)` | 唯一决定状态的位置：日志 + 进程 + error.log + profile 比对；`reconcile` 处理隔离复跑 |
| 报告 | `Report` | 汇总、`exit_code`、`failed_nodes`、JSON、JUnit |

`ProjectFacts` 是普通数据而非 trait：CLI 从 Foch 分析结果填充 tag、事件、effect、trigger 集合。`AiMode` 默认 `off`，保证结果确定；只有测试明确写 `ai=on` 才让 AI 行动，且这样的用例不与其他用例共用一局。关闭 AI 的具体机制须先在真实游戏中验证，再由 runner 声明 `ai_off` 能力；不支持的 runner 会被拒绝，而不是改为开着 AI 运行。

## 分阶段计划

| 阶段 | 内容 | 需要的验证 |
| --- | --- | --- |
| P1 | node id 与选择、`#skip/#ignore/#xfail/#mark`、expect 子句拆分（协议 v2）、收集期静态检查、`--last-failed`/`--fail-fast`/junit | 单元测试 + 一次有界真实游戏回归，覆盖子句级 PASS/FAIL 映射与 xfail/xpassed |
| P2 | `tests/` 目录、fixture 与依赖、`check` 前置条件、多步骤测试、测试辅助事件 | **先**真实验证 EU4 不加载 `tests/`；再验证多检查点的日期与顺序 |
| P3 | `#parametrize`、`--jobs`、`foch.toml [test]` | 并行实例的隔离与清理；展开预算 |
| P4 | `#fuzz` 有预算生成与重放后缩减；MTTH 自然触发（见作者工具链设计中的记录） | 统计方法预先确定 |
| 研究 | 共享会话、读取实际数值 | 状态回滚与变量输出的可行性证据 |

每一阶段都保持：注解文件可直接被 EU4 加载、未运行或运行出错永不算通过、断言之外的成功（smoke、xfailed）单独计数。本框架的结果不替代 `cargo acceptance` 的合并质量门槛。

## 待定问题

1. 修饰注解作用于下一个 `#test`（Rust 属性风格）还是整个事件（pytest 类装饰器风格）？本文推荐前者，加显式 `scope=event`。
2. `tests/` 目录名是否与已有 mod 约定冲突，是否需要可配置。
3. `default_time` / `default_tag` 是否值得引入：它缩短注解，但让单个注解不再自解释。
4. 多步骤测试中第一个失败之后是否继续推进时间（当前提案：继续，但只报告首个失败为主因）。
