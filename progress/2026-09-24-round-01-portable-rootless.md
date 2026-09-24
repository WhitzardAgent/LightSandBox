# Progress — 2026-09-24 实现轮 01:T00/T01/T03 + Portable Rootless Spike + ADR 0001

> 状态:本轮已收尾。HEAD `2138549` @ `spike/portable-rootless`,工作树干净。
> 基线:`dev/check fast` = 97 tests / 0 failures / 0 blocked;`packctl validate` OK;
> `dev/check_native.py`/`dev/check_seeds.py`(python3.12)OK。
> 下一轮入口:`python3 tools/packctl.py context T41`(T00–T03+ADR 独立 review 后置 verified)。

---

## 0. 本轮任务(用户指令)

首发宿主定位 **Portable Rootless Host**。完成 T00 + T01 + 正式 T03 +
Portable Rootless Runtime Feasibility Spike;门禁通过可进 T41/T04,被证伪则停在证据;
另:把项目持续维护到 GitHub(remote: WhitzardAgent/LightSandBox)。

## 1. 交付概览(commit 序列)

| commit | 内容 |
|---|---|
| `bc69c5c` | baseline(v5 交付包 + 开发环境) |
| `5b95372` | 设计:HostCapabilityTier 三分层 + Benchmark Task Parity / General Docker Semantic Parity 区分 |
| `b92736c` | **T00** Rust workspace 4 crate + `dev/check`(argv-only,2/1/3/4 退出码合同)+ harness 14 测 + CI |
| `957fde7` | **T01** 11/11 上游源码正式锁(tree hash + license 字节 + role;`--check-release` 门禁 rc=0) |
| `95fadff` | **T03** machine-readable HostReport(hostreport/1),portable-rootless profile 绑定证据 sha256 |
| `2138549` | **Spike 证据 + ADR 0001**(9 份 `spike/results/*.json` + `docs/adr/0001`) |

## 2. Spike 结论(全部来自真实执行,无 mock)

工具链:/opt/src 锁定源码静态构建(CGO_ENABLED=0,无 seccomp——宿主缺 libseccomp 头文件)
- runc `41b74772b651` sha256 `a531415e…08f6ec`
- umoci `f5d1219acaf6` sha256 `ae203284…e300de`
- 所有沙箱以真实非特权 uid 65534 运行(euid=0 走 rootful cgroupfs 路径必死,已留证据)

| 实验 | 证据 | 结论 |
|---|---|---|
| fixtures A/B(从零构建,无 docker) | `spike/results/fixtures.json` | A 单用户全负载;B 多 UID + setuid + device node + USER 1000:1000 |
| umoci unpack --rootless | `unpack.json` | ownership 全部 flatten 0:0(原属主存 xattr);setuid 保留;device→空文件+警告 |
| 首次执行 + /proc 形状 | `firstrun.json` | P0 新鲜 proc EPERM(外层策略);runc≥1.4 拒绝无 /proc;**rbind-ro 宿主 /proc 形状可执行** |
| P1 真实负载 | `p1.json` | bash/python3(pid 1)/git/node v22/gcc -O2 全部工作;/proc/self→宿主 pid;33 宿主 pid 可见 |
| P2 交叉暴露(安全实验) | `p2.json` | cmdline+fd 计数可见;**environ EPERM(秘密不泄漏)**;kill/ptrace 均 ESRCH |
| single-ID 多用户 | `uids.json` | SUPPORTED 5 / EMULATED setuid / **BROKEN:chown→unmapped、su、USER 1000**(未做 USER→0 暗改) |
| rootfs 两路径 | `rootfs.json` | **FULL_COPY WORKS**(≈0.9 s/972 MB、跨启动持久、删 0.2 s);TMPFS_COW 机制 proven(需 supervisor 架构块) |
| A/B 隔离矩阵 | `isolation.json` | **13 轴,0 INTERFERABLE**;3 元数据轴 VISIBLE_READONLY |
| 性能基线 | `perf.json` | prepare 0.8–1.0 s;create+exec 52–185 ms(热 ≈55–75);delete 187 ms;idle RSS 272 kB;**常驻 supervisor=0** |

## 3. 架构决策(ADR 0001,docs/adr/0001-portable-rootless-runtime.md)

**v1 backend = 选项 B:native rootless runc 可行 + 显式降级能力。** 未选:
A(全 parity 不诚实)、C(替代 launcher 不需要,supervisor 是补充不是替代)、
D(PRoot 触发条件未满足;/proc 与 multi-UID 均非 blocker)、
E(被隔离矩阵证伪)。

显式降级(必须进合同,不得静默):
1. tier=PORTABLE_ROOTLESS;2. single-ID(多 UID BROKEN 如实报告,`SingleIdCompatibilityPolicy` 为显式开关);
3. HardResourceLimits=UNSUPPORTED(RLIMIT 仅进程级护栏,RLIMIT_NPROC 禁当 per-sandbox 配额);
4. /proc 默认 rbind-ro 宿主 proc(VISIBLE_RDONLY 元数据;environ/内容不暴露),版本化 policy;
5. 静态 runc 无 seccomp 过滤(userns+最小 caps+noNewPrivileges 兜底);
6. network=none(私有 netns 仅 lo);pasta/slirp 可选未来;
7. **supervisor 架构块**:runc detached 后无驻留监督(载荷 ppid=1),生命周期观察/TMPFS_COW 接线/清理需要一个 per-sandbox 轻量 supervisor——T04 前置。

## 4. T41 八项门禁判定:**PASS**(可进 T41)

真实启动隔离 workload ✅|rootfs 隔离 ✅|生命周期监督 ⚠️(以架构块如实记录,不算绕过)|
无交叉控制 ✅|cleanup ✅|/proc 策略已定 ✅|single-ID 限制已定 ✅|no-cgroup 行为已定 ✅

**T41/T04 本轮未实质开工**:T41 为完整合同冻结包(T41-C01..C05,allowed: contracts/v1,
tests/contracts, crates/ls-core/src/contracts);仓库规则要求 T00–T03+ADR 先独立 review 再更新
verified,故停在门禁通过点,不做假进度。

## 5. 可选未做项(均记录因由)

- PRoot:触发条件不满足(/proc 未暴露秘密;multi-UID 属预期降级);GPL-2.0-or-later 已定界为可选外部进程后端
- pasta/slirp4netns:不阻塞 network=none 主承诺,未来网络包处理
- 磁盘 overlay:设计排除 + 宿主 EPERM 证据

## 6. 遗留 / Blocked

1. **GitHub push 需凭证**(无 gh/ssh/PAT;remote WhitzardAgent/LightSandBox 已配置,push 挂起)
2. T00–T03 + ADR 的**独立 review** 未做(verified 更新的前置,不得自mark)
3. T41 是下一包第一优先;`launch/CONTINUE.md` 已写交接
4. 已知非回归:直接 `python3`(=3.10)跑 check_native/check_seeds 报错 = 仓库 Python≥3.11 既有门槛;用 /usr/bin/python3.12 均 OK
5. 宿主人为固有项:cgroup 委托、subuid/newuidmap、/dev/fuse、私有 proc、seccomp 过滤——按 tier 合同降级,不绕过
