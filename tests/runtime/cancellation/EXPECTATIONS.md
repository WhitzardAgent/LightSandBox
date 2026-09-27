# T11 取消 / 超时 / 逃逸后代 — EXPECTATIONS

任务卡:`plans/tasks/T11.md`。范围:`crates/ls-guest/src/init/mod.rs`(guest 协议 v2.1)、
`crates/ls-host/src/exec/{mod,background}.rs`、`crates/ls-host/src/runtime/{backend,native/mod}.rs`
(收据 supervision 面)、`crates/ls-host/tests/cancellation.rs`、`tests/runtime/cancellation/`(本文件)。
规范钉点:docs/v5/03_DOMAIN.md §命令与退出、§取消与超时;08_FAILURES.md §取消、§退出证据;
ADR 0001 A1(/proc 宿主 rbind 只读,v1 不声称 private_proc)。

## 实现形状(guest 协议 v2.1)

- **监管降级链(诚实递减,不再 SpawnFailure)**:`--nested-pidns` 请求的
  `unshare(CLONE_NEWPID)` 失败**不再是** v2 的 SpawnFailure 降级——先尝试
  `prctl(PR_SET_CHILD_SUBREAPER,1)`,成功即 `Subreaper` 模式,再失败落 `Direct`。
  纯函数 `supervision_mode(requested, unshare_rc, prctl_rc)` 表驱动可测;
  `establish_supervision` 为薄 syscall 壳,stderr 如实报告所落模式。
  v2 的 `setsid_escape=required → unshare 失败即 SpawnFailure` 策略被 v2.1 **取代**:
  降级还要 article,不许假死。
- **`--ctrl <abs-host-path>` 杀开关**:非阻塞 waitpid + 20ms 轮询 ctrl 文件;
  `cancel\n` 前缀写 → exact-once SIGKILL 直系子(ESRCH 容忍),随后继续收割以留下真 waitpid 证据。
- **逃逸后代 drain(仅 Subreaper 模式)**:payload 终了后进入 1s 预算的 drain 窗口:
  WNOHANG 收割被收养的孤儿;`kill_adopted_descendants` 对 /proc 中**当前 ppid==me** 的表项
  SIGKILL(绝不打整容器、绝不猜进程组)。证据走 diag sidecar `<receipt>.drain.log`,
  **收据本身保持纯净**。
- **/proc 保真自校验 `proc_is_faithful`**:`/proc/self/stat` 首字段(pid)必须 == `getpid()`。
  宿主 rbind /proc 展示的是**宿主进程表**,pid 是外 namespace 的数——按 `me` 瞄
  准要么打不中、要么更糟误伤同号宿主进程。失真即静默退化为 wait-only + residual_risk。
  (自校验单测锁定。)
- **收据 supervision 字段**(host/guest 同步发布,非冻结域):
  `"supervision":{"mode":"nested_pidns|subreaper|direct","reaped_escapes":N,"residual_risk":B}`。
  host 侧 `InitReceipt.supervision: Option<SupervisionInfo>`,absent→None(老 witness 兼容),
  present-malformed→strict None(不裁断)。
- **fork-bomb 超窗**:drain 预算耗尽仍有活后代 → `residual_risk=true`,永不隐瞒。

## 宿主真相链(2026-09-27 实测,key kernel 5.15.0-119)

本宿主三重事实决定 C01 的可达语义:

1. `runc exec` 以 MINIMAL_CAPS 进容器,**无 CAP_SYS_ADMIN** →
   `unshare(CLONE_NEWPID)` EPERM(errno 1)→ 落 Subreaper(diag 收据实证)。
2. 容器 `/proc` 是**宿主进程表的只读 rbind**(ADR 0001 A1 故意设计;jupyter/tmux/宿主 init
   全可见,drain sidecar 实证)→ witness 的 ns pid 与宿主 pid **永不匹配**,kill-sweep 0 命中。
3. subreaper **收养本身工作正常**(waitpid r=0 证明有活收养子;宿主独立 python 实验复现
   PR_SET_CHILD_SUBREAPER 收养语义)→ 能收养、不能瞄准。

**结论**:nested pidns EPERM + /proc 宿主 rbind 双重约束下,in-guest 逃逸树清理在本宿主
**原理性不可达**。逃逸后代存活,但仍封在沙箱边界内(宿主 UID map + 无 capability),
最终由 sandbox delete(runc kill-all + delete)兜底全灭。

## C01 判据的如实映射(blocked ≠ 删判据)

REAL C01 不断言"逃逸树全灭",断言 **degraded 诚实语义**:

- 超时归因为 `Timeout`,退出证据为 witness 真 waitpid `Signaled{9}`;
- 收据 `supervision.mode=="subreaper"`(unshare 被拒:降级,绝不造假);
- 收据 `supervision.residual_risk==true`(sweep 无法收敛:**必须**标记,不许声称干净树);
- 新前台 exec `kill -0 <esc.pid>` 观察到逃逸者 **alive**(越杀存活,沙箱内受控);
- sentinel 旁证:同沙箱无关长活后台 exec **不受交叉 kill**(alive);
- capability planner 侧表达:`process_tree_wipe=UNSUPPORTED@this-host-native`。

宿主若未来放行 nested pidns(userns 全链或 CAP_SYS_ADMIN),同一测试应收紧回
residual_risk==false + 逃逸者 dead——判据文本已在断言注释中钉死该映射理由。

## host 侧(exec/background)

- 后台执行注册表 + 预算 horizon;取消归因 `Cancelled` vs `Timeout` 分维。
- **fold 防御**:Running 占位(chain spawned 即 promote,语义保留)与 witness SpawnFailed
  竞速时,不走非法边 fail_start,而是 `finish_unknown()` + 真相留在 outcome + eprintln 留痕
  (`reconcile_unknown(StartFailed)` 是合法终正通道;不改冻结转移表)。
  单测 `bg_spawn_failed_after_promote_settles_unknown_and_keeps_truth` 守死。
- ctrl 文件通道:host 写 `cancel\n`,guest 25ms 轮询;取消后凭 witness 收据裁断退出,
  永不凭文本猜测。

## REAL fixture 防线(三次同型事故后的根治)

三份 REAL 门禁(cancellation/exec/lifecycle)的 `real_gate` 统一带:

- **staleness 检查**:`target/debug/lsguest-init` mtime < guest 源 mtime →
  **SKIP + eprintln 提示按 musl 契约重建**(`cargo test -p ls-guest` **不刷新主 bin 产物**)。
- **ELF 静态检查**:纯字节扫 PT_INTERP,动态链接产物 → SKIP(裸 `cargo build` 会把
  debug 产物回退成动态链接;fixture 无 libgcc_s,PID1 加载必败)。
- 权威重建(witness 正式 target = musl,ADR 0001 A4):
  `cargo build -p ls-guest --target x86_64-unknown-linux-musl`。

## 证据(真实宿主,2026-09-27)

宿主:PORTABLE_ROOTLESS 全链(65534:65534),cgroupfs 只读,runc 钉装
`/opt/toolchains/lightsandbox/bin/runc`,witness 为 musl static-pie
`target/x86_64-unknown-linux-musl/debug/lsguest-init`(ADR 0001 A4)。

### 真实层(LSB_T11_REAL=1;`cargo test -p ls-host --test cancellation -- --test-threads=1`)

**4/4 通过,15.9s**(串行)。逐卡:

| 卡 | 实测 |
|---|---|
| T11-C01 | 逃逸(dummy 双 fork 孤儿 busy-loop)+ sentinel 并存;预算 2s 触发 horizon,`Timeout` 归因;witness 收据 `Signaled{9}` + `mode=subreaper` + **`residual_risk=true`**;逃逸者 `kill -0` alive(沙箱内受控,delete 兜底);sentinel alive 无交叉 kill;取消 sentinel 归因 `Cancelled`。超时杀开关被忽略时 host 侧 eprintln "ignored witness kill-switch; fell back to group kill" 如实留痕。 |
| T11-C02 | client 断开(不取消)→ 后台 exec 继续 Running,可查询,副作用文件持续增长;最终取消正常。 |
| T11-C03 | 副作用已发生后取消 → 归因 `Cancelled`,**不重跑**(副作用恰好一次);终态重放逐字节一致。 |
| T11-C04 | 并发取消/超时/查询竞速 → 终态稳定单值,无跨 execution 误杀,无非法转移。 |

### 单测

- ls-guest:**19/19**(降级链表驱动、proc 保真自校验、drain/sweep、ctrl 三元组、
  direct+kill→residual_risk、legacy 859 重写为 `nested_pidns_request_degrades_and_payload_runs`)。
- ls-host lib:**118/118**(background 注册表/horizon/归因、fold SpawnFailed 防御、
  parse_receipt supervision absent/malformed/present 三态、既有全部)。

### 门禁(权威解释器 `/usr/bin/python3.12`)

复跑命令见末节;本轮提交时全量数字以 progress/2026-09-27-round-04.md 为准。

### 如实披露

- C01 在**本宿主**为 degraded 语义(上节映射),不是 nested-pidns 全灭语义;
  该限制来自宿主 capability,不是实现省略。UML Kernel backend 的真 PID ns
  是另一条可能闭合此判据的路径。
- drain sidecar 为 evidence 通道,receipt 不进打点;host 侧仅在杀开关被忽略/降级时
  eprintln,不静默。
- 后台执行注册表为进程内存 v1;崩溃即失,耐久归 T43 supervisor。
- T09-C01 宿主 hard limit 仍 BLOCKED(cgroupfs ro),不在本卡范围。

## 门禁命令(复跑)

```bash
cargo build -p ls-guest --target x86_64-unknown-linux-musl          # fixture 权威重建(ADR 0001 A4)
cargo test -p ls-guest                                              # 19/19
cargo test -p ls-host --lib                                         # 118/118
LSB_T11_REAL=1 cargo test -p ls-host --test cancellation -- --test-threads=1 --nocapture
cargo test --workspace
/usr/bin/python3.12 dev/check_native.py
/usr/bin/python3.12 dev/check_seeds.py
/usr/bin/python3.12 -m unittest discover -s tests/pack
```
