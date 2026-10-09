# EU4 真实事件机制探针

这是研究 fixture，不是生产 CI 集成。本目录保留 2026-10-02 的研究夹具和原始结果；结果中的旧路径及哈希属于当时的运行身份。研究解释移至私有 `notes/research/eu4-headless-testing.md`。当前产品接口与实测边界见 [公开使用说明](../../../../../../../docs/foch-runtime-tests.md)。

`events/` 与 `common/` 构成一个独立测试 mod。其余文件供外部控制器使用，不覆盖原版文件。

## 复现布局

1. 准备隔离的 EU4 1.37.5 运行层和空 profile；使用已验证的 Wine、微软 D3DX DLL、Xvfb 与软件渲染配置。不要在原游戏或 Workshop mod 目录写测试文件。
2. 把本目录的 `events/`、`common/` 放到测试 mod 根目录。profile 下创建 `mod/foch-probe.mod`，`path` 指向该目录；`dlc_load.json` 的 `enabled_mods` 包含 `mod/foch-probe.mod`。
3. 把 `commands.txt`、`mechanics-start.txt` 放到可写游戏运行层根目录。`commands.txt` 是控制台命令；`mechanics-start.txt` 是 effects，二者不可混用。
4. 直接启动 `eu4.exe -start_tag=SWE -auto_run=commands.txt -seed=42 -debug`，用 `userdir.txt` 和 `-userdir` 指向相同独立 profile。原版无 Steam 警告需要外部确认；本轮的 Win32 helper 源码在本机 `target/eu4-headless-research/ack-warning.rs`，未注入游戏。
5. 启动判定器，读取该 profile，而非历史日志：

```text
python judge.py PROFILE --wait 300 --checks event_immediate modifier_applied modifier_present_day_2 delayed_event_state delayed_event_completed modifier_expired
```

返回 0 表示检查完成并通过，1 表示检查失败，2 表示不完整。判定结果写 `probe-result.json`。它只判定约定的检查日志；还应单独收集引擎 error.log、崩溃和环境诊断。

`--stop` 仅写 `controller-stop` 文件，需外部启动器消费并结束自己的游戏实例；它不直接杀进程。没有配套启动器时不要认为游戏会自动退出。到期事件只输出 END，游戏可能继续推进少量 ticks，完成日期取 END 记录。本机已验证的启动器为 `target/eu4-headless-research/run-linux.sh`。

## 检查含义

- 1444.11.11：事件设置 flag、变量 7，并添加 5 天 modifier；事件内部检查 modifier。
- 1444.11.13：延迟事件检查 flag 和变量，检查 modifier 仍存在，并留下完成 flag。
- 1444.11.21：检查延迟事件完成、modifier 不再存在，输出 END。

它验证十天内的具体行为，不是长期稳定性、自然触发概率或玩家事件选项测试。变量检查是 `>= 7 && < 8`，适用于这里已知的整数写入。

负例实测采用 effects 中 `limit = { always = no }`，失败分支输出 `FOCH_CI_FAIL always_no`；即使随后出现 END，判定器也返回 1。`results.json` 保存本次正例、负例和最终机制用例记录及夹具身份，不包含游戏二进制。
