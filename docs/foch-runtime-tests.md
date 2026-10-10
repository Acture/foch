# 同文件 EU4 事件测试

状态：本地工作树实现，尚未发布。框架提供收集、编译、内置运行器和结果判定；`foch test` 自动定位游戏并启动，用户无需自备运行器。内置运行器、关闭 AI、按条件拆分的检查已在真实 EU4 1.37.5（原生 Windows）端到端验证。真实用户目录保护目前靠备份+按内容校验/恢复（内容必定一致，修改时间可能变）；共用一局和托管 CI（Linux + Wine）尚未验收。

## 编写测试

在原生事件前添加注释即可；源码不需要预处理，EU4 会忽略测试注解。

```text
namespace = reforms

#mark(reform)
#test(time=1444.11.11, tag=SWE, name="reform grants flag",
#     effect={ clr_country_flag = reform_done },
#     expect={
#         has_country_flag = reform_done
#         NOT = { has_country_flag = reform_failed }
#     })
#skip(reason="needs a war fixture")
#test(time=1444.11.11, tag=SWE, name=at_war, expect={ is_at_war = yes })
country_event = {
	id = reforms.1
	title = none
	desc = none
	picture = none
	hidden = yes
	is_triggered_only = yes
	immediate = { set_country_flag = reform_done }
	option = { name = OK }
}
```

每行测试描述都必须是 `#` 注释。连续多个 `#test` 可测试同一个事件。当前只支持下一项顶层 `country_event`，要求 `hidden=yes`、`is_triggered_only=yes`，不自动操作弹窗选项，也不证明自然触发条件正确。

只有 `#test(time=1444.11.11, tag=SWE)` 时，报告明确标为 `smoke`，只验证在指定国家完成调用链路。要验证行为，添加 `expect`；非空游戏错误日志会阻止通过。

| 参数 | 含义 |
| --- | --- |
| `time` | 必填，精确到天的原始开局日期，如 `1444.11.11`。执行端必须真正加载对应状态；修改日历数字不算开局。 |
| `tag` | 必填，三位大写国家标识，如 `SWE`。 |
| `name` | 可选，同一事件上唯一的用例名；空格用引号包围。省略时按位置编号 `#0`、`#1`，增删注解会改变编号。 |
| `effect` | 可选，在所选国家执行的原生准备 effects，可以调用作者自己的准备事件。 |
| `advance_days` | 可选，默认 0，上限 36500。调用后等待真实游戏天数再检查，不改变日历值。 |
| `expect` | 可选，在所选国家求值的原生 trigger block。每个顶层条件单独检查和报告，全部为真才通过。 |
| `ai` | 可选，默认 `off`，保证结果确定。只有测试需要 AI 决策时写 `ai=on`。 |
| `session` | 可选，`auto`（默认）允许与兼容用例共用一局；`alone` 总是单独一局。 |
| `player` | 可选，`yes` 表示该国必须是玩家国家，因此不与其他用例共用一局。 |
| `use` | 可选，fixture 名列表。fixture 定义在 `tests/` 中，CLI 目前尚不收集 `tests/`。 |

修饰注解写在 `#test` 之前，只作用于紧随其后的那一个 `#test`：

| 注解 | 含义 |
| --- | --- |
| `#skip(reason="...")` | 不运行，报告为 skipped。 |
| `#ignore(reason="...")` | 默认不运行；`--include-ignored` 一并运行，`--ignored` 只运行这些。 |
| `#xfail(reason="...", strict=yes)` | 断言预期失败。只吸收断言失败；崩溃、超时、记录不完整仍然失败。`strict=yes` 时意外通过算失败。必须有 `expect`。 |
| `#mark(war, slow)` | 标签，用 `-m` 选择。 |

顺序固定为：加载日期和国家 → 准备 effect → 调用目标事件 → 等待 advance_days → 判断 expect。跨日检查通过延迟游戏事件实现，不能保证与其他同日事件、月度更新的先后顺序。即时检查不承诺所有派生数值已重算。

## 命令

```text
foch test ./my-mod --collect-only
foch test ./my-mod -k "reform and not slow" --collect-only --format json
foch test ./my-mod -m war --collect-only
foch test "./my-mod/events/reforms.txt::reforms.1::reform grants flag" --collect-only
foch test --api
foch test --api advance_days
foch test ./my-mod --no-run --out ./compiled-tests
foch test ./my-mod --out ./test-results --timeout 300
foch test ./my-mod --game-path "D:/Games/EU4"
```

`PATH` 可以是 mod 根目录、其 `events` 目录、一个事件 `.txt` 文件，或 `文件::事件[::名字]`。目录发现范围为 `events/**/*.txt`。用例地址形如 `mod名::events/reforms.txt::reforms.1::名字`，mod 名取 `descriptor.mod` 的 `name`，否则取目录名。

`-k` 是关键词表达式（`and`、`or`、`not`、括号），每个词不区分大小写地匹配用例地址的子串；`-m` 是同样语法的标签表达式。零用例、无效注解或筛选无匹配均返回非零退出码。

收集、API 查询和 `--no-run` 不启动游戏，也不需要 Steam 或 Foch 的全局 base-data 配置。`--no-run` 与 `cargo test --no-run` 相同，只把每一局的测试层和清单写到 `--out`，不复制源 mod。输出目录必须新建或为空，而且位于源 mod 外。测试没有单独的 `foch compile` 命令；`compile` 留给以后需要编译成原生脚本的扩展。

`test` 自动收集、生成测试层并用内置运行器启动真实游戏，用户不需要自备运行器。游戏安装通过 `--game-path`、Foch 配置、Steam 依次自动定位；都找不到才报错。

### 内置运行器

运行器只读加载游戏安装：它在短路径下搭一个运行层，复制 `eu4.exe` 和松散文件、把数据目录链接过去，再用一次性用户目录启动游戏（只启用源 mod 和生成的测试层），默认关闭 AI 保证确定性。它只启动自己的游戏进程，看到每个用例的结束记录或超时后结束进程，然后拆掉运行层。

能力由运行器自身决定，用户无需声明；用例需要运行器做不到的事，Foch 在启动任何游戏前拒绝整次运行：

- 开局日期：目前只支持经真实游戏验证的 `1444.11.11`；其他日期的用例被拒绝。
- 关闭 AI：用 `ai` 控制台命令实现（已验证 AI 行动降为 0）。
- 共用一局：尚未在真实游戏验证，暂不启用，用例各自单独一局。

### 真实用户目录保护

游戏启动时会重写真实用户目录里的少量设置文件。运行器先备份这些文件，运行结束后按内容校验，内容若变化就从备份恢复，并把该局判为 `runtime_error`——因此内容必定与运行前一致（游戏可能改动它们的修改时间，这是无害的）。默认目录是 `文档/Paradox Interactive/Europa Universalis IV`，可用 `--eu4-user-dir` 指定。

要做到游戏完全不碰真实目录（连修改时间都不变），需在文件 API 层重写路径（把游戏对真实用户目录的 `CreateFileW` 路径改写到一次性目录）；这条路线和曾经尝试并放弃的进程内重定向，记录在私有 `notes/research/eu4-headless-testing.md`。

### 共用一局

运行器声明支持共用一局时，Foch 才考虑合并。同时满足这些条件的 `session=auto` 用例才放进同一局：开局日期相同、`ai=off`、不要求玩家国家、国家不同、静态分析能界定其影响范围且彼此不重叠、不涉及全局 flag。任何疑问都让用例单独运行。`--collect-only` 显示每个用例所在的一局及单独运行的原因；`--isolate` 让所有用例单独运行。共用一局时失败的用例会自动单独重跑，以单独运行的结果为准；单独运行通过时报告标出用例间干扰。

## 一局的产物与判定

Foch 按"一局"运行，每局在 `--out` 下有独立目录 `session_NNNN`（隔离重跑为 `rerun_NNNN`），里面有该局的 `bundle.json` 清单（`protocol` 2、`run_id`、`mod_name`、局内用例、生成文件、启动参数、开局与结束日期、按顺序的检查列表、每个用例的起止日期、所需能力）、生成的测试层和游戏写出的 `userdir/logs`。生成的测试 mod 名形如 `<mod 名> tests [3fa2c1]`，事件命名空间带 mod 名和哈希。

游戏日志中每条记录为 `FOCH_TEST_V2 <run> <局摘要> c<用例> <记录>`，记录为 `BEGIN`、`PASS <检查>`、`FAIL <检查>` 或 `END`。框架严格校验身份、日期、顺序和完整性。结果状态：

| 状态 | 含义 |
| --- | --- |
| `passed` / `smoke` | 全部检查成立 / 无 expect，调用链完成 |
| `xfailed` / `xpassed` | 预期失败如约失败 / 意外通过（strict 时算失败） |
| `failed` | 有 expect 条件不成立，报告指向该条件的源码行 |
| `setup_error` | 国家 scope 或 fixture 前置条件不成立 |
| `incomplete` / `protocol_error` | 记录不完整 / 身份、日期、顺序或清单不一致 |
| `runtime_error` | 启动失败、退出非零、超时、缺日志、`error.log` 中出现归因于生成测试层的条目，或真实 profile 内容被改动 |

超时后 Foch 结束自己启动的游戏进程。输出根目录保存 `result.json`（汇总、每局启动详情和每个用例的结果）与 `junit.xml`。只得到布尔 trigger 结果，不伪造内部状态数值。

EU4 每次启动都会往 `error.log` 写入与测试无关的环境条目（缺本地化、其他 mod、原版告警），因此"非空"本身不算失败。判定按条目归因：引用了生成测试层（唯一命名空间或测试 mod 名）的条目才让该局 `runtime_error`，其余条目仅作为提示列出、不改变判定。

被测 mod 在其 `descriptor.mod` 里声明的依赖会按安装注册表（用户目录下的 `mod/*.mod`）解析并一并加载：依赖先于被测 mod、测试层最后，`dlc_load.json` 据此排序。传递依赖按"被依赖者在前"的顺序展开；未安装的依赖会告警并在没有它的情况下继续。

启动时若上一次运行被强杀而没能清理，残留的运行层会在超过宽限期后被清扫；`Ctrl-C` 会在当前这一局拆解后停止，不留下游戏进程或运行层。

示例夹具在 `src/packages/foch-test/tests/fixtures/runtime-tests`，包含冒烟、即时断言、延迟两天断言和一个故意失败的断言。因此全量运行这组夹具预期返回非零。

## 代码位置

注解识别、参数类型与编辑器补全在 `src/packages/foch-annotation`；用例含义、计划、生成和判定在 `src/packages/foch-test`；启动真实游戏、运行层、一次性 profile 和真实目录保护在 `src/packages/foch-runner`；CLI 只负责发现、文件输出和展示；`foch lsp`（`src/packages/foch-lsp`）只依赖注解库。设计见 [测试框架 API 设计](foch-test-framework-design.md)。

## 编辑器与后续范围

`foch lsp` 使用同一份注解定义为 `#test`、`#skip`、`#ignore`、`#xfail`、`#mark` 和 `#parametrize` 提供参数补全、悬停和注解诊断。跨注解的规则（同名用例、`xfail` 缺少 `expect`）只在 `foch test --collect-only` 时报告。当前没有注解内部原生 effects/triggers 补全、测试运行按钮或失败跳转操作。

已实现：CLI 收集 `tests/` 中的多步骤测试和 fixture（依赖 EU4 不加载顶层 `tests/` 的目录模型，真实游戏实测待补，合并侧的识别另行处理）、`#parametrize` 参数化、基于内置目录的 effect/trigger 静态检查、被测 mod 声明依赖的加载。尚未实现：从基础快照补全静态检查的国家与事件集合、fuzz、`--last-failed`、独立 `foch lint`、书签以外任意日期的内置开局准备，以及平台托管 CI 的真实验收。已有本地运行环境只验证真实 `1444.11.11` 开局。

## 已记录的真实游戏验证（协议 v1）

2026-10-02，以当时的 `foch test` 从示例源码收集并编译四个用例，运行真实 EU4 1.37.5：冒烟、即时 flag 断言、延迟两天 flag 断言通过，`always=no` 的故意失败断言失败。调用日期为 `1444.11.11`，延迟检查日期为 `1444.11.13`。四份 `error.log` 均为 0 字节，加载的注解源码与仓库夹具逐字节相同，整体退出码 1 与负例一致。

运行使用本地 WSL 内独立 profile、Wine/Xvfb/llvmpipe 和 4 个 CPU 核，复用已有运行材料及缓存；单例游戏启动到清理约 59–64 秒。这是虚拟显示上的实际游戏，仍然渲染，不是无渲染引擎，也不是托管 CI 冷启动测量。

最初一轮正确拒绝了缺少 title/desc/picture 的生成事件：游戏调用标记虽然完整，错误日志并非空。补齐字段及回归测试后得到上述结果。证据见 [机器验证记录](../src/apps/foch-cli/tests/probes/eu4-runtime/foch-runtime-tests-2026-10-02.json)，其中的旧路径和程序身份保留原样。该记录验证的是协议 v1 与当时的程序。

## 内置运行器的真实游戏验证（2026-10-07，原生 Windows）

在原生 Windows（非 WSL/Wine）上，用 `foch test` 内置运行器跑通了协议 v2：

- 逐项实验确认了运行层做法：复制 `eu4.exe` 和 DLL、链接数据目录后启动可行；短路径避开了 `MAX_PATH` 导致的贴图缺失；不经 Steam 启动（设 `SteamAppId`）不弹警告；`-userdir` 在原生 Windows 生效。
- 关闭 AI 的控制台命令是 `ai`：同样推进两天，AI 行动次数从 1053 降到 0，测试仍通过。
- 端到端：`foch test <fixture> -k delayed_flag` 运行真实 EU4 1.37.5，`delayed_flag` 用例的 scope、invoked、1444.11.13 的 expect 全部按日期通过，退出 0；运行前后真实用户目录的 `settings.txt`、`pdx_settings.txt`、`dlc_load.json`、`game_data.json` 四个文件内容逐字节相同（运行器备份+校验保证）。
- 曾尝试进程内注入让游戏完全不碰真实目录，未成功，已放弃；调查与"改用 `CreateFileW` 文件 API 层重写"的后续路线记录在私有 `notes/research/eu4-headless-testing.md`。

尚未验证/未完成：游戏完全不碰真实目录（需文件 API 层方案）、共用一局、任意开局日期、托管 CI（Linux + Wine）。原游戏安装目录在以上所有运行中文件数不变。

产品方向见 [作者工具链设计](./foch-authoring-design.md)。这些事件测试不替代 `cargo acceptance` 的合并质量验收。
