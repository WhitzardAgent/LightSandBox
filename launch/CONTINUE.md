> v5：架构/接口以docs/v5与contracts/v1为准；旧seed仅为局部逻辑参考。当前产品任务目录已更新为45项。

# 交接 2026-09-29（真实 benchmark case runner，优先 SWE/Terminal）

当前分支增加普通用户 worker case runner、原生 OCI create/exec/files/delete
驱动与原始证据收集。worker 地址放在被忽略的
`dev/runtime-workers.local.json`，公开仓库不保存内部 SSH 目标。

- SWE-bench `full:astropy__astropy-11693` 已在候选后端真实运行。gold patch
  经锁定上游 grader 判定 resolved；原始 pytest 摘要 49 项，错误 patch 在
  verifier 前失败；两次运行均有清理收据。状态只能写
  `CANDIDATE_ONLY_POSITIVE / REFERENCE_NOT_RUN`。
- Terminal-Bench `atrx-vep-crispr` 的原版 agent Dockerfile 在隔离 rootless
  BuildKit 中因 `/etc/gshadow` GID 42 无法由单 ID userns 表示而失败。
  solution/verifier 尚未运行，不能声明支持。下一步接通已实测的 UML 多 UID
  路径来构建原版 agent/verifier 镜像，再执行原 solution 和 `tests/test.sh`。
- CyberGym 已完成 agent 工作区、原 build 与清理证明，但按当前推进优先级暂停；
  不研究或生成有效 PoC。

本轮本地门禁：packctl 34、native 76、seed 109、runtime runner 7、docs 180、
examples 20 均通过。macOS 上直接运行 `cargo test -p ls-host` 有 14 项 Linux
宿主相关失败；Linux worker 的 workspace 测试在本轮代码的早期版本通过，最新
case-driver 已通过 `cargo check`，仍需在 SSH 稳定后补一次完整 Linux 回归。

真实证据在 `evidence/runtime/ws03/`；它证明指定 HostReport 上的候选运行，不是
Docker 参考差分、完整 benchmark parity 或独立认证。

# 交接 2026-09-24(实现轮:T00/T01/T03 + Portable Rootless Spike + ADR0001)

本轮完成(commit 序列见 git log):
- bc69c5c baseline → 5b95372 设计(HostCapabilityTier 三分层 + parity 语言区分)→
  b92736c T00(Rust workspace 4 crate + dev/check + harness,unit 39/fast 97 绿)→
  957fde7 T01(11/11 上游源码锁,/opt/src 干净 clone,--check-release rc=0)→
  95fadff T03(HostReport hostreport/1 + portable-rootless profile 绑定 sha256)。
- Portable Rootless Runtime Spike(feasibility-only,代码在 spike/,未并入 crates/contracts/CI):
  用户态静态构建 runc(41b7477,无 seccomp,宿主缺 libseccomp)/umoci(f5d1219);
  fixtures A/B 从零构建;umoci rootless 元数据实测(flatten+xattr+device 警告);
  首次真实 rootless 执行(uid 65534,rbind-ro 宿主 /proc 形状);P1 全负载绿;
  P2 安全实验(environ 经内核 dumpability 阻断不泄漏;cmdline/fd 元数据只读可见;
  signal/ptrace 均 ESRCH);single-ID 多用户分类(3 BROKEN 如实记录,无 USER→0 改写);
  rootfs 两路径(FULL_COPY WORKS;TMPFS_COW 机制 proven,需 supervisor 架构块);
  隔离矩阵 13 轴 0 INTERFERABLE;性能基线全部落盘。
- ADR:docs/adr/0001-portable-rootless-runtime.md——决策 = 选项 B
  (native rootless runc + 显式降级能力);T41 八项门禁判定 PASS
  (生命週期监督以"架构块"如实列项,不算绕过)。

下一包:T41(协议/合同冻结,packctl context T41,allowed: contracts/v1, tests/contracts,
crates/ls-core/src/contracts;5 项 T41-Cxx 测试)。开始 T41 之前须完成 T00–T03+ADR 的
独立 review(仓库规则:不同责任人 verified 才能更新)。PRoot 触发条件未满足,未做;
pasta/slirp 属可选,未阻塞主线;GitHub push 因无凭证仍挂起(用户已知)。

验证基线(本轮末):dev/check fast = 97 tests 0 failures;packctl validate OK;
直接 python3(3.10) 跑 check_native/check_seeds 失败属已知 Python≥3.11 要求,
用 python3.12 均 OK。

# 后续会话继续指令

继续LightSandbox，保持AGENTS的目标和安全边界。先读取当前Git差异、state/project-status.json与最近交接，运行packctl validate和next。选择一个依赖已verified的任务，运行context并只加载其必要设计/代码/测试。

核对上一会话证据是否绑定当前commit，不依赖“完成”文字。若有未提交用户修改，保护它们，不重置或覆盖。每次只处理当前任务，使用失败测试→最小实现→真实验证→独立review的循环。

缺少工具、权限、source lock、任务资产或测试worker时按事实标BLOCKED，不能静默skip或使用mock当真实验收。不改变测试分母、官方verifier或安全profile来获得通过。

按任务卡保存原始证据和handoff。只有集成者根据可信CI与不同责任人的review更新verified。当前卡通过且授权范围允许时可继续下一个ready任务；否则明确停点和最小下一步。
