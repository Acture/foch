# Tribute Adjust — foch native plugin

第一个 foch first-party native 插件:把每个属国给宗主的**最终上供数值乘一个百分比**,
不修改 `tribute_income_modifier` 等 modifier 本身。解决 modifier 堆高后属国太穷、
上供失去意义的问题。

## 配置维度

- **全局系数** `global_percent`(0–200)
- **按属国类型** `kind_percent`:键是 `common/subject_types` 里的类型名
  (`vassal`、`eyalet`、`core_eyalet`、`tributary_state`、`daimyo_vassal`,以及 Mod
  自定义类型),不是固定的 9 种枚举。未写本类型时沿 `copy_from` 向父级找,遇到重复键
  即停;链上没有 `default` 时再看 `kind_percent["default"]`。命中的值**替换**更低优先级,不叠加
- **按 tag** `tag_percent`:三字 tag → 百分比,替换整条类型链

优先级:`tag_percent[tag]` > `kind_percent[本类型]` > `kind_percent[父类型]` > … >
`kind_percent[default]` > `global_percent`。`enabled = false` 时全部按 100% 直通。

## 状态

| 里程碑 | 状态 |
| --- | --- |
| 插件骨架(manifest、init、REFUSED 路径) | 完成 |
| 系数解析 + 配置合并(含单测) | 完成 |
| 朝贡签名表 1.37.5(静态逆向 + 唯一性机器验证) | 完成,详见 `the signature table in signatures/` |
| 朝贡支付执行器 detour(`FUN_140b1c800` 命令金额缩放) | 完成 |
| 宿主端到端测试(真实 foch host + fake exe,active/refused 双例) | 完成,`tests/host_e2e.rs` |
| vassal/colony 收入类别号 | 待运行时识别:`AddIncome` hook pattern 已就绪,先挂日志再启用缩放 |
| **真机数值验证(朝贡)** | **完成**:MNG 1444+60 天(ai=on 跨年):100% ≥195 / 50% [175,185) / 0% [140,160) |
| **真机数值验证(附庸)** | **完成**:ENG 释放威尔士(base_tax+8 放大信号),vassal_share 0.4→0 [55,70) vs 0.4→0.8 [70,85) |
| tag ↔ 国家 映射 | `tribute_executor` 仍读 `CCountry+0x20`。`subject_income` 的第 3 参数改走省份 id 向量（`+0x88/+0x90` → `context+0x1ca8` → `CProvince+0x468`），宗主自己的省份身份会跳过；未实机回归 |
| 回执 | 两条热路径写 `FOCH_RECEIPT/1`（`case`/`request`/`type`/`kind`/`tag`/`scale`/`before`/`after` 全是字符串）。`test-mod` 用 `expect_receipt` 加 `numeric` 区间；`foch test --collect-only` 与 `--no-run` 不启动游戏 |
| 滑块面板 | 独立 `tribute-panel`（不注入、不挂钩 DX11）。Save 写入 `tribute-config.json`，插件每 250ms 防抖热加载 |

签名表条目 `unverified` 时插件 init 会 REFUSE 并说明缺口——绝不按猜测 hook。
当前表内三个 pattern 均已在本 exe 内验证唯一匹配(exe sha256 随表锁定)。注意:hook 点必须满足宿主
"14 字节处恰为指令边界"的 prologue 约束——金额 getter `140b1a180` 不满足(边界 12/16),执行器
`140b1c800` 满足,故 hook 挂在执行器上。

## 构建

```sh
cargo build -p foch-eu4-tribute --release
# 产物 foch_eu4_tribute.dll 与 foch-plugin.toml、signatures/ 一起即一个插件包
# 独立滑块（不注入游戏）:
# tribute-panel --data-dir <插件数据目录> --root <游戏目录> --root <mod目录>
```

## 部署

```sh
foch plugin import mods/tribute-adjust
foch plugin enable dev.foch.eu4-tribute-adjust
foch plugin plan && foch plugin launch
```

## 运行时配置

manifest config(`enabled`、`global_percent`、`tag_overrides` JSON 字符串)为基线;
插件数据目录下的 `tribute-config.json`(完整 `TributeConfig` JSON)覆盖之,是滑块面板
的持久化格式。插件在结算热路径上每 250ms 至多查看一次该文件的修改时间,变更后刷新
进程内配置,不必重启游戏。

## 为新游戏版本补签名

1. 对目标版本 Steam eu4.exe(IDA/Ghidra)定位:月度结算中把属国 tribute 加进宗主
   收入的函数;从该函数参数读属国关系对象的类别判别值与 tag 字符串的方式。
2. 填 `signatures/<version>.toml`(pattern 用宿主 `find_pattern` 语法,记录 provenance),
   确认唯一匹配后把 `status` 改 `verified`,填 `exe_sha256`。
3. 在 `src/signature.rs` 中 `include_str!` 新表并按 `game_version` 选择;
   manifest `game_versions` 同步收紧到该版本。


