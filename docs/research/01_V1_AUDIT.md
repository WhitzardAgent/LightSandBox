# 对上一版规划的审查与修正

本版保留独立包、OCI/runc、Rust/Python、普通VM与全量任务目标，修正会误导实施的承诺。旧版只作历史资料，开发以本包为准。

| 旧版风险 | 本版处理 |
|---|---|
| 开始前读完全部文档 | 短AGENTS、按任务context，根入口与深层规范分开 |
| 只有大阶段，没有可核验工作包 | 33张有依赖/范围/接口/具体验收的任务卡与状态机 |
| 对OpenSandbox字段的推断可能当成已确认协议 | T01锁源码/SDK/实际参考，观察清单不充当完整契约 |
| 以参考通过子集为100%分母 | 保留完整集合、reference/asset/oracle阻塞分别登记 |
| 假定每个Cyber任务已有成功PoC | 单列oracle可用性，缺口不算通过 |
| 替换Cyber agent便暗示全栈无Docker | T26代理与T27验证runner拆开 |
| 共享rootfs/page cache被描述为Docker所无的优势 | 纠正，Docker已有；优劣通过相同条件测量 [S12] |
| 坚持绝对无每sandbox helper | tiny init与必要broker可用，常驻开销实测，正确性先于数量承诺 |
| API/DB重启可以自动恢复所有PTY | 明确FD实际拥有者，broker死亡不能恢复原FD [S11] |
| 内核能力仅靠存在检查或uname | 主动effects probe，安全关键能力不降级 |
| 资源限制/磁盘配额过于笼统 | required硬约束无法兑现则拒绝，du监控不算硬quota |
| rootfs快照隐式包含workspace卷 | 根层、外部卷、RAM分开列出和验证 |
| 性能最后才开始测 | T05/T08起留基线，T29再优化 |
| 初期十几个Rust crate | 4个主体crate起步，内部按职责分模块 |
| Coding Agent可无限root测试 | 普通开发与受控worker分离，测试租约和保护判据 |

## 前提核验仍需要实际机器

本轮没有访问用户VM，不能确认root权限、cgroup控制器、LSM、配额、网络权限或libc/binary兼容。没有完整下载上游源码和任务资产，不能冻结完整commit/镜像digest/数据集revision。它们是T01–T03的明确交付，不能用规划文档替代。

## 与旧文件的对应

原00–08设计主题由 `docs/design/01–11`承接；原09–13测试/发布主题由 `docs/acceptance/`承接；原14风险/ADR、15来源、16工作拆分分别进入research、catalog/tasks和harness。原Master Prompt分为BOOTSTRAP/CONTINUE/REVIEW/DEBUG/RELEASE，不再要求一次读完全部材料。
