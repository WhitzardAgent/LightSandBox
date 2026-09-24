# 镜像、网络、存储与安全实现细则

## Dockerfile到进程

Dockerfile+context经独立BuildKit的Dockerfile frontend生成OCI镜像；BuildKit本身有OCI(runc)worker，不需要containerd worker。[S04] 获取manifest/config/layers后验digest和platform，按OCI规则应用层，生成rootfs与runtime config。ImageSpec不提供网络、完整Compose、进程监督或用户自定义Docker CLI行为。[S01/S03]

流水线：InputSnapshot → BuildResult/ImageRef → LaunchSpec → RuntimeBundle → runc create → 网络及解析文件准备 → runc start → 按profile观察运行/健康。OCI create之后不得再修改config.json期待生效。[S01]

一份Image config的ENTRYPOINT/CMD/USER/ENV/WORKDIR映射还不足以证明Docker一致。必须覆盖空值vs缺失、USER用户名/UID/GID和附加组、PATH查找、shm、ulimits、capabilities、StopSignal、镜像VOLUME初始填充、Docker healthcheck扩展、mount propagation及信号。exec spec不能继承构建器的宿主环境。

## 启动策略

image模式使用原始进程生命周期，即使一瞬间退出也按真实结果报告；idle-agent模式显式注入init并覆盖入口。数据库/HTTP sidecar正常使用image模式。OpenSandbox/Harbor适配层按参考后端的入口策略编译，记录配置差异。不要用同一个统一sleep命令覆盖所有镜像。

## 构建上下文和供应链

使用BuildKit/成熟OCI工具处理层和whiteout；不自行实现会跟随恶意symlink的解包循环。输入context在授权根目录内建立不可变快照或明确失败，不边读变化目录边声称可复现。

`.dockerignore`、Dockerfile专属ignore、多阶段、ARG、build secrets、cache mount、remote ADD/frontend均属构建合同。旧安全bundle seed拒绝symlink的行为只是受限示范，不能因此宣称完整Docker context兼容。完整支持应复用成熟context/OCI工具，支持合法链接但防越界。

构建器单独网络预算/凭据与scratch存储，不把宿主~/.docker完整暴露给任务。非固定remote frontend或新增entitlement要求operator授权；secret不进入hash日志/镜像层。BuildKit cache有owner/secret作用域，不能跨租户泄漏。

## rootfs / volume / snapshot

首个路径使用目标VM实际支持的成熟snapshot方式，按引用共享只读内容。OverlayFS/FUSE/copy不是无差别替换：权限、xattr、文件能力、硬链接、inode、rename、删除标记和磁盘配额需要分别验。fallback只有满足相同语义和安全要求才可自动使用，否则拒绝。

volume和rootfs分开索引。镜像VOLUME自动创建/初始copy-up由参考模式决定；bind mount只能来自已授权资源或制品，不能从外部HTTP传任意宿主路径。tmpfs内存、/dev/shm、inode和日志存储都参与预算。

snapshot首发只承诺指定rootfs/卷的文件一致点，不保存RAM/进程/TCP。运行中snapshot必须报告crash-consistent或quiesced，跨数据库/多服务一致性由应用协调，不能通过顺序复制宣称原子。

## 网络

CNI只负责网络连通性基础，不自动提供完整服务DNS或域名白名单。[S14] 网络子系统必须拥有IPAM分配记录、DNS服务、端点、规则revision和清理记录。使用独立命名空间与产品自有规则链，禁止覆盖宿主其它产品规则。

native attachment与egress分开：attachment=isolated/group，egress=deny/public/allowlist。Harbor的public/no-network等按其固定语义转换；native默认拒绝外网，Harbor默认值可能不同，必须显式映射并检查操作员策略。[S10/S13]

IP/CIDR策略与hostname策略分开宣告。域名解析为IP并放行不能证明请求仅访问该域名：共享IP、DNS变化、直接TCP、IPv6、UDP/QUIC都可能破坏期望。严格域名策略需要受控出口代理和禁止旁路；不支持的协议拒绝，并接受这会影响某些任务兼容。动态策略更新需处理既有连接与conntrack，API返回生效revision必须在实际执行后。

端点暴露默认认证反向代理，限定端口/protocol/sandbox代次。不要把容器私网IP当可用公网endpoint，也不要把中央管理API key传到guest服务。

## 资源和宿主

检测namespace/cgroup/seccomp/文件系统等真实能力，首发只选择实际可验证配置。cgroup v2优先，v1/rootless是单独支持项。`storage_bytes`硬限制无可用配额时拒绝，而非后台轮询假装hard limit。PIDs、FDs、mount、network states和CAS inode也有全局预算。

“不换内核”保留；但如果已有内核缺必要原语或安全策略不允许，则报告HOST_UNSUPPORTED。静态Rust、旧glibc构建、KVM缺失属于不同维度，不能互相推导兼容。

## 威胁模型

共享内核沙盒以VM作为高风险负载外层边界。不能以共享kernel容器承诺抵御任意kernel exploit。API恶意路径/归档、任务root用户、恶意构建、过量输出、endpoint越权、管理令牌泄漏均应纳入威胁矩阵。runc/hooks/seccomp等文件只由operator控制；来自任务的OCI hooks不能直接作为宿主可执行代码。

更强默认安全可能使原benchmark行为变化。reference-faithful与hardened测试分别报告，安全增强不能悄悄放入parity数据。
