# 验证策略：合同、环境行为和性能分别证明

## 分层

L0 文档/清单：链接、schema、状态机、任务依赖、API方法矩阵。L1 无特权参考：类型、argv、HTTP请求、超时、重试、退出/未知、CLI解析。L2 真Linux运行：runc、mount、cgroup、network、FD、PTY。L3 上游互操作：原版Harbor/OpenSandbox。L4 完整基准差分。L5 故障/隔离/吞吐。

L2 首发宿主配置为 `PORTABLE_ROOTLESS`；FULL_ROOTLESS/ROOTFUL 差分另行分层。每项宿主能力判定取 `SUPPORTED`/`DEGRADED`/`UNSUPPORTED`/`NOT_TESTED`，绑定证据命令、退出码与关键输出（见 T03 HostReport 与 `specs/host-profiles.json`)；能力未证不支持时对应声明作废，不得降级视为通过。

本包执行L0/L1及旧参考回归，不执行L2–L5。L1的MockTransport证明HTTP客户端请求/错误行为，不能证明服务实现；旧FakeEnvironment不能证明真实Harbor继承接口兼容。

## 新增的具体边界测试

argv保持空参数和shell元字符；shell必须明确解释器；时间整数/无bool；native核心拒绝verifier；数组null/[]区别；客户端close不发DELETE；写请求中断只发一次；幂等key返回用户；generation header；跨源redirect拒绝；wait timeout不cancel；任务取消只停止等待；unknown必须有收据才能确认；原版接口漂移不能靠AST通过。

## 环境差分重点

ENTRYPOINT/PID1/StopSignal、USER/附加组、默认PATH、mount卷copy-up、/dev/shm、ulimits、文件能力/xattr、只读根、procfs、网络DNS/IPv6/UDP、动态策略现有连接、服务依赖/healthcheck、独立stop/copy、GDB/ptrace、native argv与shell、PTY信号、超时双重fork、volume和GC并发。

容差在执行前固定，不能为了候选增加时间、改变网络或扩大内存。stdout/stderr不要求跨流绝对全序，但每流字节顺序和退出收据必须一致。非确定性任务重复实验并报告，不归一化掉真实差异。

## 性能口径

cold image/build、warm image/cold sandbox、warm resident和active workload四种阶段分开。报告p50/p95/p99创建/执行/清理延迟，成功任务/单位时间，CPU/cgroup实际内存/PSS/磁盘/inodes/PIDs/FDs/mounts，失败率与泄漏。不能只报空闲resident数量。

先和同镜像Docker最小容器比较，再和固定OpenSandbox backend比较。BuildKit、公用代理和日志存储的资源也计入系统总成本。若tiny-supervisor更可靠但开销接近containerd shim，结论如实；优化不得通过移除可靠性功能获胜。

## 收敛门禁

协议变化需要同步OpenAPI、JSON schema、CLI/SDK方法矩阵、Harbor映射、golden请求和错误测试。每个release清单从逐任务JSON重算，来源未锁、无真实宿主测试、无完整manifest任何一项都阻止最终100%声明。
