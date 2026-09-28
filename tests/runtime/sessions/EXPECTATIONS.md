# T13 持久Shell及命令边界 — EXPECTATIONS

任务卡:`plans/tasks/T13.md`。范围:`crates/ls-api/src/sessions/`、`crates/ls-host/src/sessions/`、
`tests/runtime/sessions/`(本文件)。规范钉点:docs/v5/03_DOMAIN.md §资源与生命周期(Session 行、
generation fencing、删除后不复用 ID)、docs/v5/06_HTTP.md §输出与重放(payload 字节与事件类型分离)、
docs/v5/08_FAILURES.md(诚实 Lost/拒绝,不伪造成功)。

## 实现形状(本任务落地的单元层)

- **wire 词汇(ls-api::sessions,纯 serde,无 I/O)**
  - `create_session` body:`{kind:"shell", shell, cwd?, env?}` — kind 封闭枚举(今天只有 shell;
    `pty` 是 decode 拒绝,不是同义词;PTY 会话 wire 面归 PTY 任务链,不偷猜)。`shell` 为 guest 绝对
    解释器路径(沿用 ExecSpec shell 规则)。env 名规则同 exec.schema(非空、无 `=`、无 NUL),值无 NUL。
  - `run_in_session` body:`{script}` 严格单键白名单;script 非空、无 NUL、≤64KiB。script 是喂给
    常驻解释器 stdin 的 shell INPUT — 持久 shell 绝不靠重 exec / 拼接历史命令模拟。
  - `SessionRefWire`:session_id(镜像 ls-core 域 id 规则;升入 ls-core::domain::ids 是指定合同
    owner 步骤)+ sandbox_id + generation + kind + state(`open|exited|closed` 封闭词汇)。
  - **控制通道独立于用户 stdout(卡片输出要求)**:`ControlFrame`(tag="kind",deny_unknown_fields;
    `command_started{seq}` / `command_finished{seq, exit, observed?}` / `session_exited{exit}`)与
    `EncodedChunk`(session_id/stream/offset/payload_b64,base64 严格)是两个**不可互转**的类型。
    命令结束与退出码只能经 ControlFrame 到达;任何 payload 字节(包括逐字节等于真实控制帧
    序列化的伪造文本)都是惰性 payload(T13-C02 单元臂)。
  - Fail-closed decode:未知字段/未知 kind/标量类型错误(bool 当 seq)/显式 null(null ≠ missing,
    冻结 execspec 纪律)一律 INVALID_ARGUMENT 并点名字段。
- **会话真值(ls-host::sessions,纯逻辑,不 spawn 不碰进程)**
  - C01 持久化:session 只有一个常驻 shell;cwd/env 变化靠**同一进程**保留。宿主只经
    `CommandFinished.observed`(全量替换)获知 post-state;无 observation ⇒ 什么都不变
    ("无观察"绝不等于"从输出猜"或"编造未变")。`Dispatch` 携带派发时刻快照证明持久语义。
  - C03 并发:命令按登记顺序进 FIFO(`register` 发 session 内单调 seq),`dispatch` 最多一个
    active。策略确定:`Serialize{max_queued}`(有界等待队列,溢出 = 显式 `Busy`)或
    `RejectWhileBusy`(任何第二个在途命令 = 显式冲突)—— 恰是 03_DOMAIN 的"按登记顺序执行
    或明确冲突"两种合法答案。兄弟 session 零共享(seq/env/计数器各自封闭)。
  - C02 单元臂:`accept_output` 只按 stream 计字节,**不读内容**;伪造全套控制帧字节只动计数器,
    active/cwd/env/state 不变;随后真实 ControlFrame `ShellExited` 仍能转移状态(非空测试)。
  - C04 句柄失效:`ShellExited` 按登记顺序 abort(active 先,queue FIFO 后)再 seal 为 `exited`;
    此后除 `close` 外一切操作 = `Exited` 错误。worker 顺序律:先 drain 管道再 post ShellExited,
    迟到的 output 在本层是协议错误。
  - 显式清理:`close` 回收记录并**烧掉** id(删除后不复用);重开烧过的 id = `BurnedId`;
    每个 control frame 先做 generation fencing(世代不符在任何状态读取前拒绝)。

## 单测证据(2026-09-28,macOS,rustc 1.93.1)

- ls-api `sessions::` **13/13**:create_body_defaults_and_validates、
  create_body_rejects_unknown_fields_kinds_and_illegal_values、
  create_body_golden_json_shape_is_stable、route_material_is_boundary_checked、
  run_body_script_rules_closed、session_id_mirrors_domain_id_rules、
  session_ref_wire_round_trip_and_closed_state_vocabulary、
  chunk_keeps_non_utf8_payload_byte_exact、
  chunk_rejects_oversize_bad_base64_and_wrong_shape、
  control_frame_round_trips_every_variant、control_decode_is_fail_closed、
  command_finished_json_is_tagged_and_disjoint、
  payload_carrying_every_control_serialisation_stays_inert(C02 wire 臂)。
- ls-host `sessions::` **14/14**:open_defaults_and_register_rejects_bad_scripts、
  duplicate_open_and_policy_validation、commands_serialize_in_registration_order、
  queue_cap_overflow_is_explicit_busy、reject_while_busy_policy_conflicts_explicitly(C03)、
  cwd_env_persist_across_commands_via_observation(C01)、
  command_without_observation_changes_no_recorded_state、sibling_sessions_share_nothing、
  payload_is_accounted_never_interpreted(C02)、
  shell_exit_aborts_every_handle_in_registration_order(C04)、
  started_frame_requires_the_active_seq、
  generation_fencing_rejects_stale_frames_before_any_state_read、
  close_is_explicit_cleanup_and_burns_the_id、pty_payload_label_is_rejected_for_shell_sessions。

TDD 留痕:ls-api 先红(模块未接线,collected=0)→ 绿 13/13;ls-host 先红
(0/14,`not yet implemented: T13` panic)→ 实现后 13/14(真实抓到测试自身
`b"\x03\x1b[2J"` 5 字节误写为 4)→ 修正后 14/14。

## 验收卡登记 — runtime 双腿 BLOCKED(无授权 worker,不删判据不造假)

本仓库当前没有产品 runner(T00 任务);`./dev/check runtime --task T13 --worker WORKER_ID`
与 `--differential` 需实际授权 worker。本包(macOS 笔记本)上不存在该平台,且
runc/guest 双腿需要 Linux dev 宿主。以下逐卡登记为 **BLOCKED**(阻塞原因写明,
不以 echo/mock/预录输冒充通过):

| 卡 | 场景 | 单元层已证 | worker 腿要证 | 阻塞原因 |
|---|---|---|---|---|
| T13-C01 | 先 cd/export 再发下一条命令 | 持久模型:observed 全量替换 → 后续 Dispatch 快照携带新 cwd/env;无 observation 不变;兄弟 session 零污染 | 真实 guest shell 内 `cd /app && export A=1` 后下一条命令 `pwd`/`env` 输出新值;普通 exec(非 session)不被影响 | 无授权 worker;需要 runc+guest 解释器循环上报观察 |
| T13-C02 | 命令输出所有已知 delimiter | wire+host 双臂:伪造全套控制帧序列化 + `exit 0` 标记经 payload 后零状态转移;真实帧随后仍生效 | 恶意命令真实输出全套 marker,超时/退出码仍由真 waitpid 收据裁断,不提前结束、不返回伪造码 | 无授权 worker;需真实进程与收据链 |
| T13-C03 | 同时提交多条改目录命令 | 序列化 FIFO + 有界队列溢出 = Busy + RejectWhileBusy 冲突语义,全部确定性 | 并发真实命中:按登记顺序执行的观察证据(或拒绝的明确回执),无交叉污染 | 无授权 worker;需并发注入面 |
| T13-C04 | exit / Ctrl-C 结束前台程序 | ShellExited 顺序 abort + 句柄全失效 + 显式清理(burn id,NotFound) | 真 shell exit 后 session 状态符合参考;Ctrl-C 杀前台进程组后 shell 存活可续跑;取消语义与 T11 归因一致 | 无授权 worker;Ctrl-C/进程组语义需真实 PTY/信号链 |

单元层收集口径:`cargo test -p ls-api sessions::`(13)、`cargo test -p ls-host --lib
sessions:: pty:: broker::` 中的 sessions 部分(14)。runtime 双腿的原始日志/退出码/收集数
在授权平台上补记;本文件不含运行器,登记即全部。

## 如实披露

- 宿主 rustc 1.93.1,仓库 dev/toolchain.json 钉 1.98.1(dev/check 的 pin 闸在本机不通过);
  单元测试在 1.93.1 下编译运行干净,无版本相关特性使用。
- ls-host 既有 14 个失败(`probe`/`images::layout`/`rootfs::*`/`runtime::native::runc`)是
  macOS 平台缺失 Linux 原语的**既有**事实,与本任务无关;本任务新增模块全绿且只用 std
  unix-可移植 API,Linux 上同样成立。
- `session resize/attach` CLI 形状仍在 surface 台账 planned;本任务只冻结
  create_session/run_in_session 单元面,未把 PTY 会话 wire 偷渡进 kind 词汇。
