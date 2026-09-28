# T14 PTY、终端控制与FD生命周期 — EXPECTATIONS

任务卡:`plans/tasks/T14.md`。范围:`crates/ls-host/src/pty/`、`crates/ls-host/src/broker/`、
`tests/runtime/pty/`(本文件)。规范钉点:docs/v5/03_DOMAIN.md(Session 行、generation 用于实例
代次/fencing、PID 不是稳定资源身份)、09_RUNTIME_DETAILS.md(网络/资源段落的预算律;输入为 runc
console-socket 固定语义、PtyRef、generation、有界 streams)。

## 设计冻结(原文落在两个模块顶部的 rustdoc;改动需合同 owner)

- **角色**:PTYBroker 进程持有 PTY master fd;API 不持有 master,只经本地通道 attach/reattach,
  带身份校验(worker 腿 = unix socket SO_PEERCRED;单元层 `PeerCred{uid,gid}` 建模范式,
  **pid 故意缺席** — PID 不是稳定身份)。
- **诚实 Lost**:broker 死亡 ⇒ PTY 是带原因的显式 `Lost`。**绝不按编号重开、绝不从 DB 整数
  伪造 FD** — 唯一合法 fd 来源是存活 broker 句柄。"重启 shell 模拟 attach"被否决为新会话,
  不是旧 PTY(卡片原话)。死亡模型是类型级的:`PtyBroker::drop` 未经 `close()` 即崩溃 ⇒
  id 烧毁为 `Lost{BrokerDied}`;干净 `close()` 烧毁为 `Closed` — 两者是不同答案;id 在任何
  generation 永不复用(03_DOMAIN 删除后不复用 ID 律)。
- **类型化控制,绝不嗅探字节**:resize / window-change / Ctrl-C / half-close 是
  `PtyControlFrame` 封闭集合。`\x03`(ETX)、`\x04`(EOT)、CSI resize escape 等终端字节是
  DATA — 本层与 broker 不存在任何读取 payload 的代码路径(测试注入字节汤实证零误触发)。
  Resize = 权威设定(SizeSet,worker 腿做 TIOCSWINSZ+SIGWINCH);WindowChange = 客户端
  窗口变化通知(SizeAdvisory);两者都更新记录尺寸,**新 attach 重放最后记录尺寸**。
- **半关闭是方向性的**:关某 attachment 的 INPUT 不动其 OUTPUT;master stdin 仅在最后一个
  可输入 attachment 关闭后 EOF,且 EOF 不可撤销(新 attach 也写不回)。
- **FD 泄漏是词汇级错误**:一切 fd 计费走 `FdLedger::acquire`(全局 cap,耗尽 = 带
  live/cap/requested 数字的显式错误,绝不静默超卖),`FdLease` RAII 恰好释放一次;
  `by_ref` 表让"关 A 不误伤 B"(T14-C04)在词汇层可证。真实 /proc fd-diff 是 worker 腿。
- **重连即原 PTY**:断连 = detach;再 attach 得新 attach seq(registry 全局单调)+ 重放尺寸;
  输入/输出字节计数 per-PTY 连续(offset 规则的生产面归 T12 spool,本层证明 broker 侧不重置)。

## 单测证据(2026-09-28,macOS,rustc 1.93.1)

- ls-host `pty::` **8/8**:pty_ref_rules_mirror_domain_ids_and_fence_generation、
  frame_constructors_bound_dimensions_fail_closed、
  terminal_state_tracks_resize_and_window_change_distinctly、
  terminal_state_ctrl_c_counts_and_half_close_is_idempotent、
  fd_ledger_cap_is_explicit_and_reclaimable、fd_ledger_multi_slot_and_sibling_isolation(C04
  词汇臂)、fd_ledger_deterministic_churn_proves_no_leak(确定性伪随机 240 轮,逐步不变式,
  终态恰 0)、fd_charging_never_exceeds_cap_even_for_multi_slot_requests。
- ls-host `broker::` **12/12**:register_then_attach_with_owner_identity、
  attach_wrong_generation_is_explicit_fencing_rejection、
  attach_identity_check_matches_owner_uid、
  broker_death_is_honest_lost_and_the_ref_is_burned(Lost+烧 id+stale grant 惰性+fd 归零)、
  clean_close_distinct_from_lost、register_never_shadows_a_live_id、
  typed_control_drives_terminal_state_and_reattach_replays_size(C02 尺寸重放臂)、
  payload_bytes_are_counted_never_sniffed(字节汤零误触发)、
  half_close_is_per_attachment_and_eof_only_when_last_input_leaves、
  fd_cap_pressure_is_explicit_never_silent(C04 压力臂)、
  sibling_ptys_never_share_fate(C04 兄弟臂)、
  reattach_after_client_disconnect_continues_same_pty(C02 连续性臂)。

TDD 留痕:pty 先红(未接线 collected=0 + 首次编译抓到 `Result<FdLease,_>` 断言形态错,
改为比较 err 侧)→ 绿 8/8;broker 先红(0/12,`not yet implemented: T14` panic;
另 22 处 guard 类型不可 PartialEq/比较的编译错;1 处测试自身漏绑 grant 导致真 fd 计费
断言偏移 → 捕获后修正)→ 绿 12/12。

## 验收卡登记 — runtime 双腿 BLOCKED(无授权 worker,不删判据不造假)

`./dev/check runtime --task T14 --worker WORKER_ID` 与 `--differential` 需实际授权
worker(runc/PTY 平台);本包无该平台,寄存如下:

| 卡 | 场景 | 单元层已证 | worker 腿要证 | 阻塞原因 |
|---|---|---|---|---|
| T14-C01 | GDB 断点/Ctrl-C/continue/resize | 类型化 Ctrl-C=InterruptForeground 计数、尺寸设定/通知分离、字节汤零误触发 | 真 PTY 上 GDB `break; run; ^C; continue` + `stty rows cols` 与参考(GNU Linux 参考实现/上游 spec)交互与信号逐点一致 | 无授权 worker;需 console-socket 真实 PTY + termios/进程组 |
| T14-C02 | 命令运行中断开 client 并重启 API | broker 侧:断连不重置计数、重放得新 seq+尺寸重放、输入累计连续 | API 进程真实重启后向存活 broker 重 attach;offset 规则(T12 spool)在重连后继续且不错位 | 无授权 worker;需跨进程 API/broker + UDS SO_PEERCRED |
| T14-C03 | 强杀 PTY 拥有者再查询 | drop-handle 崩溃模型 ⇒ `Lost{BrokerDied}` 显式、id 烧毁、stale grant 惰性、fd 归零、Closed 与 Lost 区分 | kill -9 broker 后经 API 查询返回显式 Lost 与恢复限制说明;绝不透明恢复 | 无授权 worker;需真实多进程死亡传播 |
| T14-C04 | 多次 open/attach/resize/close 后查 FD | 词汇层:cap 显式耗尽(带数字)、churn 终态恰 0、by_ref 兄弟隔离、death 全回收 | 真实 /proc/self/fd diff:循环 open/attach/resize/close 无累积泄漏;sibling 会话 fd 不受牵连 | 无授权 worker;/proc fd 观察需 Linux 平台多进程 |

单元层收集口径:`cargo test -p ls-host --lib pty:: broker::`(8+12=20)。
runtime 双腿证据在授权平台上补记;本文件不含运行器,登记即全部。

## 如实披露

- 宿主 rustc 1.93.1 vs 仓库钉 1.98.1(dev/check pin 闸本机不通过);新代码只用 std
  (`Arc/Mutex/HashMap/BTreeMap`,无 libc、无 tokio、无 unsafe),macOS 与 Linux 行为一致。
- 生产面未接:console socket 接 master、SO_PEERCRED、TIOCSWINSZ/SIGWINCH/SIGINT 派发、
  T12 spool 接线均属后续运行时任务;本层刻意无可执行副作用,任何"已可运行 PTY"的说法
  都不成立(诚实口径:单元真值,非运行真值)。
