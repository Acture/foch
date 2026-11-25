![Tribute Adjust — Live 0%–200% Subject & Tributary Slider Panel](workshop/thumbnail.png)

# Tribute Adjust (`foch-eu4-tribute`)

> Steam Workshop companion mod & store copy: [descriptor.mod](workshop/descriptor.mod) · [foch.toml](workshop/foch.toml) · [English BBCode](workshop/STEAM_WORKSHOP_EN.bbcode) · [中文 BBCode](workshop/STEAM_WORKSHOP_ZH.bbcode)

**Tribute Adjust**（插件 ID：`dev.foch.eu4-tribute-adjust`）是基于 Foch 原生插件宿主（`foch-runtime-host`）的《欧陆风云 4》（EU4）Windows x64 原生 DLL 插件。它按可配置百分比（`0`–`200`%）直接缩放每个属国向宗主国缴纳的**最终朝贡与附庸月度收入金额**，而不修改 `tribute_income_modifier`、`vassal_income` 等脚本修正本身，从根源上解决中后期修正堆高后属国财政破产或上供比例失衡的问题。

---

## 配置维度与优先级

所有百分比配置均限制在 `0`–`200`（即 `0%` 至 `200%`）范围内，超出区间的值会自动截断。当总开关 `enabled = false` 时，所有属国一律按 `100%`（`1.0x`）原样直通。

插件支持三个层级的系数配置：

1. **按国家 Tag（`tag_percent` / `tag_overrides`）**：三位国家 Tag（自动规范化为大写 ASCII，如 `KOR`、`MUG`、`WLS`）对应的百分比，直接覆盖该属国所属的类型链。
2. **按属国类型（`kind_percent` / `vassal_percent`）**：键为 `common/subject_types` 中的脚本类型名（保留脚本原始拼写，如 `vassal`、`tributary`、`eyalet`、`core_eyalet`、`tributary_state`、`daimyo_vassal` 或大修 Mod 自定义类型）。若当前类型未显式配置，则沿 `copy_from` 继承链依次向父类型查找；若整条链未命中且未包含 `default`，则再检查 `kind_percent["default"]`。
3. **全局基准（`global_percent`）**：未命中 Tag 与属国类型链时的全局默认百分比（默认 `100`）。

### 优先级规则（替换而非叠加）

```text
tag_percent[TAG]
  > kind_percent[本类型]
  > kind_percent[copy_from 父类型 1]
  > ...
  > kind_percent["default"]
  > global_percent
```

高优先级命中的百分比**直接替换**低优先级数值，各层之间绝不相乘叠加。

### 双通道缩放机制

- **朝贡结算通道（`tributary`）**：插件初始化时按基准系数修补引擎朝贡常量（`cash_rate`、`adm_rate`、`dip_rate`、`mil_rate`、`manpower_rate`），并在朝贡支付执行器（`tribute_executor`，命令金额偏移 `+0x1c`）中按残差倍率 `desired / applied_define_scale` 进行缩放。普通属国不会被双重缩放，而配置了 `tag_percent` 或热更新后的属国仍能精确命中目标百分比。
- **附庸/属国月度收入通道（`vassal`）**：插件初始化时按基准系数修补引擎 `vassal_share` 常量（原版基数 `0.4f`；若附庸基准设为 `0%` 且同时配置了非空的 `tag_percent`，则跳过常量清零以免引擎提前算出 `0`），并在属国单笔收入函数（`subject_income`）返回后对输出金额应用残差倍率 `desired / applied_vassal_scale`。

---

## 大修 Mod 兼容机制

- **开放属国类型与 `copy_from` 动态继承链（`SubjectTypeGraph`）**：
  - 属国类型键名并非硬编码的原版 9 种枚举，而是开放字符串，支持任何在 `common/subject_types/*.txt` 中定义的原版或大修 Mod 属国类型。
  - `SubjectTypeGraph::load_playset` 按 EU4 加载语义解析有序目录列表（第一个根为游戏本体目录，后续为按 Playset 优先级排列的 Mod 目录）：自动识别并应用 Mod `descriptor.mod` 中的 `replace_path`（剔除被覆盖的前置 `common/subject_types` 文件，并拒绝包含 `..` 的越界路径）；同一根目录下按规范化路径顺序读取 `.txt` 文件，同名属国类型后定义覆盖先定义（支持原版前向声明）；`copy_from` 父链遍历内置环路检测，遇到重复键自动停止。
- **`i16::MAX` 国家上限**：
  - 国家紧凑标识解码与 `CountryView` 校验统一通过 `foch_runtime::dataplane::game` 完成，国家索引上限支持到 `i16::MAX`（`32767` 个 0 基国家槽位），完全兼容注册数千个国家 Tag 的大型大修 Mod。
- **受保护的内存读写**：
  - 所有运行时内存访问均通过 `foch_runtime::dataplane::Memory` 守卫完成。若遇到空指针、未映射内存页、半初始化对象或超出范围的索引，均安全降级为 `None` 并回退至类型/全局系数，不会触发访问违例崩溃。

---

## 构建与部署

### 环境要求

- **操作系统与平台**：Windows x64
- **支持游戏版本**：Steam 版 EU4 **1.37.5**（`foch-plugin.toml` 与 `signatures/signatures-1.37.5.toml` 严格绑定该版本的 `eu4.exe` SHA-256 摘要与签名表；遇到其他版本或未验证签名时插件会在初始化阶段直接 `REFUSED`，绝不盲目挂钩）

### 1. 编译 CLI、宿主与插件

在仓库根目录下构建 Foch CLI（`foch-cli`）、Windows 原生插件宿主（`foch-runtime-host`）以及本插件包（`foch-eu4-tribute`）：

```powershell
cargo build --release -p foch-cli -p foch-runtime-host -p foch-eu4-tribute
```

构建完成后会在 `target/release/` 下生成：

- `foch.exe`：Foch 命令行工具
- `foch_runtime_host.dll`：原生插件宿主 DLL（与 `foch.exe` 位于同一目录时，`foch plugin launch` 会自动发现并装载为独立运行时层的 `VERSION.dll`；也可通过 `--host-dll` 显式指定）
- `foch_eu4_tribute.dll`：Tribute Adjust 插件主体 DLL
- `tribute-panel.exe`：独立滑块配置面板

### 2. 导入并启用插件

`mods/tribute-adjust/foch-plugin.toml` 声明的入口文件为 `foch_eu4_tribute.dll`。将最新编译的 DLL 放入插件目录后即可通过 `foch plugin` 导入、启用并启动：

```powershell
Copy-Item target/release/foch_eu4_tribute.dll mods/tribute-adjust/foch_eu4_tribute.dll

foch plugin import mods/tribute-adjust
foch plugin enable dev.foch.eu4-tribute-adjust --playset default
foch plugin plan --playset default
foch plugin launch --playset default
```

启动后可使用终端打印的运行时目录查看插件实际激活状态：

```powershell
foch plugin status --run-dir "<RUNTIME_DIR>"
```

---

## 运行时配置与独立 `tribute-panel` 面板

### Manifest 基线与 `tribute-config.json` 热重载

插件配置由两层合并而成：

1. **Manifest 基线配置**（定义于 `foch-plugin.toml`）：
   - `enabled`（`bool`，默认 `true`）：总开关。
   - `global_percent`（`int`，默认 `100`，范围 `0..=200`）：全局基准百分比。
   - `vassal_percent`（`int`，默认 `100`，范围 `0..=200`）：附庸收入分成基准百分比（载入时映射为 `kind_percent["vassal"]`）。
   - `tag_overrides`（`string`，默认 `"{}"`）：编码为 JSON 字符串的按 Tag 覆盖表，例如 `"{\"FRA\":150,\"MUG\":50}"`。
2. **运行时数据文件 `tribute-config.json`**（位于插件数据目录 `dir::DATA` 下）：
   - 保存完整的 `TributeConfig` JSON，覆盖 Manifest 基线中对应的字段：

     ```json
     {
       "enabled": true,
       "global_percent": 100,
       "kind_percent": {
         "vassal": 80,
         "tributary": 50
       },
       "tag_percent": {
         "KOR": 150,
         "MUG": 25
       }
     }
     ```

   - 插件在结算热路径上通过 `ConfigWatch` 每 **250 ms**（`RELOAD_DEBOUNCE_MS = 250`）至多检查一次 `tribute-config.json` 的修改时间与文件大小。文件更新后自动热加载进程内配置，**无需重启游戏**；若写入了损坏的 JSON，插件会保持上一次有效配置不变；若删除 `tribute-config.json`，则自动恢复为 Manifest 基线配置。

### 独立滑块面板 `tribute-panel`

`tribute-panel` 是独立于游戏进程运行的原生 Windows GUI 工具（**不注入 `eu4.exe`、不挂钩 DirectX / DX11**）。

```powershell
# 打开图形滑块面板（--root 第一个传 EU4 游戏安装目录，后续按 Playset 顺序传各 Mod 目录）
target/release/tribute-panel.exe --data-dir "<插件数据目录>" --root "<EU4游戏目录>" --root "<Mod目录>"

# 在命令行直接打印解析后的 copy_from 类型树与当前生效系数（不打开窗口）
target/release/tribute-panel.exe --data-dir "<插件数据目录>" --root "<EU4游戏目录>" --root "<Mod目录>" --dump
```

- 面板提供 `Global 0–200` 滑块、`Enabled` 复选框、按 `copy_from` 深度缩进的属国类型列表（实时显示继承计算后的 `effective_percent`）以及当前选中类型的 `Selected type` 滑块。
- 点击 **Save** 按钮会通过临时文件（`tribute-config.json.tmp` → `tribute-config.json`）原子写入配置，游戏内的插件会在 250 ms 内自动完成热重载。

---

## 离线测试与回执校验

`mods/tribute-adjust/test-mod` 目录下包含一套使用 `#test` 与 `expect_receipt` 注解编写的测试事件（涵盖 `tribute_smoke.txt`、`income_probe.txt`、`kind_probe.txt`、`subject_probe.txt`、`vassal_scale.txt`）。

可以使用以下命令在**不启动 `eu4.exe`** 的情况下离线收集测试用例或编译测试会话包：

```powershell
foch test mods/tribute-adjust/test-mod --collect-only
foch test mods/tribute-adjust/test-mod --no-run --out target/tribute-adjust-bundles
```

插件的两条热路径在每次触发时会向宿主日志输出 `FOCH_RECEIPT/1` 结构化回执（JSON 对象内所有字段值均为字符串）：

- `case`：关联测试国家 Tag（如朝贡方 `KOR` 或宗主国 `ENG` / `FRA`，未知时为 `"untagged"`）
- `request`：固定为 `"1"`
- `type`：`"tribute"`（朝贡执行器）或 `"subject_income"`（属国月度收入）
- `status`：`"scaled"`（发生数值缩放）或 `"passthrough"`（原值直通）
- `tag`：解析到的属国三位 Tag（未解析到时为空字符串 `""`）
- `kind`：`"tributary"` 或 `"vassal"`
- `scale` / `before` / `after`：实际应用的残差缩放倍率、缩放前金额与缩放后金额（均为字符串形式，配合 `expect_receipt` 中的 `numeric` 区间断言使用）

---

## 已知限制与验证边界

为便于玩家与 Modder 评估适用场景，以下明确区分**已完成真机/宿主验证的行为**与**尚待最新实机回归或受当前架构限制的行为**：

### 已验证项

- **签名表与宿主加载（端到端已验证）**：`signatures/signatures-1.37.5.toml` 中的特征码已在 Steam EU4 `1.37.5.0`（`eu4.exe` SHA-256: `c9ec55a4783fc0f250bbf536dfd25c8ec190ed0ed621384ca501908c60ae9f8e`）中完成唯一性校验与指令边界验证；基于真实 `foch-runtime-host`（`VERSION.dll`）与 `foch_eu4_tribute.dll` 的激活（`active`）与拒载（`refused`）端到端测试（`tests/host_e2e.rs`）已通过。
- **朝贡基准缩放（已真机验证）**：明朝（`MNG`）1444 开局跨年朝贡结算在 `100%`、`50%`、`0%` 三档系数下的国库增量已在真实游戏会话中验证通过。
- **附庸收入分成基准缩放（已真机验证）**：英格兰（`ENG`）释放威尔士（`WAL`）后的 `vassal_share` 基准分成缩放已在真实游戏会话中验证通过。

### 待实机验证项与已知限制

- **`subject_income` 省份路径反查属国 Tag 尚未做最新实机回归**：
  - `subject_income`（`FUN_1403f1510`）的第 3 参数并非 `CCountry*`，而是包含省份 ID 向量（`+0x88` / `+0x90`）的对象。当前代码通过 `Eu4Game::province(id)`（`context+0x1ca8`）读取候选省份归属字段 `CProvince+0x468` 的紧凑国家标识，并跳过宗主国自身拥有的省份来确定属国 Tag。
  - 该反查逻辑与异常指针守卫已通过 Windows 合成内存（`FakeGame`）单元测试，但 **`CProvince+0x468` 候选偏移及该省份反查路径尚未在最新实机游戏会话中完成回归验证**。若实机下未能从省份列表解析出非宗主国 Tag，钩子会安全降级并回退使用 `"vassal"` 类型系数。
- **`FOCH_RECEIPT/1` 回执与 `test-mod` 尚未做最新实机回归**：
  - `FOCH_RECEIPT/1` 回执日志的格式化契约与 `mods/tribute-adjust/test-mod` 中的 `expect_receipt` 注解已通过单元测试以及 `foch test --collect-only` / `--no-run` 离线校验，但**尚未执行最新一轮带插件的实机 `foch test --with-plugins` 回归测试**。
- **运行时属国子类型细分限制**：
  - 当前宿主 `init` 尚未向插件传入当前 Playset 的根目录列表（`tribute-panel` 通过 `--root` 参数加载 `common/subject_types`，而游戏进程内的 `subject_types::ACTIVE` 默认为空），且 `subject_income` 钩子参数未直接暴露 `CSubjectType` 键名。因此游戏内两条钩子热路径当前分别固定以 `"tributary"` 和 `"vassal"` 查询类型系数；在 `tribute-panel` 中为 `eyalet`、`core_eyalet`、`appanage` 等非朝贡子类型单独设置的系数虽会写入 `tribute-config.json`，但游戏内 `subject_income` 热路径暂不能按具体子类型区分，如需对特定属国单独调优，请优先使用按国家 `tag_percent`（`tag_overrides`）覆盖。
- **启动期常量修补与 `0%` 特例**：
  - 引擎 `defines` 常量修补在 `init` 阶段执行一次，运行时热重载依靠钩子中的残差倍率 `desired / applied_define_scale` 动态生效。若启动时将 `vassal` 基准设为 `0%` 且当时未配置任何 `tag_percent` 覆盖，`vassal_share` 会被修补为 `0.0`，此时在游戏运行中途热添加非零系数需要重启游戏后才能恢复基础分成。
- **版本锁定与成就限制**：
  - 严格锁定 Steam EU4 `1.37.5`（Windows x64），其他版本或修改过的 `eu4.exe` 会被拒载；加载任何原生 DLL 插件期间无法获得铁人模式成就。
