# v4 全面审查与 v5 处置

以下问题根据实际归档文件核查。优先级描述工程风险，`设计已收敛`不等于`运行时已解决`。所有涉及Linux实际资源的修改仍须真实验证。

## A01｜阻断｜核心模型夹带验证器

**原文件：** `docs/harbor/06_END_TO_END_ONE_CLICK_FLOW.md`

**问题：** EnvironmentSpec 包含 verifier、artifact_rules，且 CLI 有 harbor verify；与独立 runtime 边界冲突。

**v5处置：** 移出 Runtime EnvironmentSpec；HarborRecipe/Trial 上层维护验证语义；核心 CLI 不提供 verify。

**验收：** 核心 schema 拒绝 verifier/reward/agent 字段。

**负责工作包：** T34/T37/T39。状态：设计已收敛；实现/验证依据测试台账。

## A02｜阻断｜三套状态枚举混用

**原文件：** `docs/design/03_INTERNAL_CONTRACTS.md; docs/engineering/02_RUNTIME_ARCHITECTURE.md; seed/python/src/lightsandbox_seed/journal.py`

**问题：** 管理操作、guest 命令和沙盒状态被不同文档用同一套 SUCCEEDED/FAILED 表达。

**v5处置：** Operation、Execution、Sandbox、Cleanup 分域；非零退出仍是 Execution.exited，退出内容独立。

**验收：** 状态机、schema、客户端测试自动对齐；unknown 不重跑。

**负责工作包：** T04/T41。状态：设计已收敛；实现/验证依据测试台账。

## A03｜阻断｜命令形式与时间单位分叉

**原文件：** `schemas/native-objects.schema.json; docs/design/03_INTERNAL_CONTRACTS.md`

**问题：** schema 仅有 command:str 和 timeout_sec，文档允许 argv/shell 与毫秒。

**v5处置：** 原生 v1 使用 tagged argv/shell、timeout_ms；Harbor/OpenSandbox 单独转换。

**验收：** 空参数、shell 元字符、秒到毫秒转换、两模式互斥测试。

**负责工作包：** T10/T36/T41。状态：设计已收敛；实现/验证依据测试台账。

## A04｜高｜严格启动与常驻 init 混用

**原文件：** `docs/engineering/02_RUNTIME_ARCHITECTURE.md; docs/engineering/06_DOCKERFILE_TO_RUNTIME_MECHANISM.md`

**问题：** 所有沙盒常驻 tiny init 与镜像 ENTRYPOINT PID1 行为不能同时作为默认。

**v5处置：** launch_mode=image 或 idle-agent；与 security_profile 正交；默认 image。

**验收：** 短命 ENTRYPOINT 必须退出；idle-agent 必须披露覆盖。

**负责工作包：** T06/T08。状态：设计已收敛；实现/验证依据测试台账。

## A05｜高｜单节点 broker 故障影响全部终端

**原文件：** `docs/engineering/02_RUNTIME_ARCHITECTURE.md`

**问题：** 单 worker 一个稳定 broker 降开销，但 broker 死亡造成广泛 FD/退出证据丢失。

**v5处置：** 首发每沙盒一个小 supervisor；其死亡隔离该沙盒并标记不确定；共享监督器另立性能 ADR。

**验收：** API重启、manager重启、supervisor失效、VM重启四类测试。

**负责工作包：** T11/T14/T43。状态：设计已收敛；实现/验证依据测试台账。

## A06｜高｜临时停止、删除、close 含义不清

**原文件：** `docs/harbor/06_END_TO_END_ONE_CLICK_FLOW.md; seed/python/src/lightsandbox_harbor/provider_core.py`

**问题：** SDK env.close、provider stop(delete) 和 runtime destroy 的资源副作用易混淆。

**v5处置：** 客户端 close 只关连接；delete 返回清理 Operation；lease.release 显式清理；不承诺 stopped 可原地重启。

**验收：** 退出客户端上下文不删除环境；delete等待清理收据。

**负责工作包：** T21/T38/T42。状态：设计已收敛；实现/验证依据测试台账。

## A07｜高｜API timeout 与执行 timeout 混用

**原文件：** `seed/python/src/lightsandbox_harbor/bridge.py`

**问题：** 以消费输出流的客户端 timeout 控制 guest 执行，会受网络等待影响并可能取消范围过大。

**v5处置：** transport_timeout、wait_timeout_ms、exec.timeout_ms 独立；等待超时不杀任务。

**验收：** 断线、慢输出、等待取消均不自动新发命令。

**负责工作包：** T11/T36/T42。状态：设计已收敛；实现/验证依据测试台账。

## A08｜高｜恢复时 generation 语义过宽

**原文件：** `docs/design/03_INTERNAL_CONTRACTS.md`

**问题：** 恢复/重建都增加代次，可能使存活 sandbox 在 API 重启后失去句柄。

**v5处置：** 原地服务重连不换 generation；替换创建新 ID；revision 用于状态更新。

**验收：** API重启旧句柄仍可用；真正替换拒绝旧身份。

**负责工作包：** T04/T21。状态：设计已收敛；实现/验证依据测试台账。

## A09｜高｜UNKNOWN 无法接受晚到证据

**原文件：** `seed/python/src/lightsandbox_seed/journal.py`

**问题：** 旧 seed 的 UNKNOWN 是终态，无法表达后续查到真实退出收据。

**v5处置：** 禁止重新启动但允许经过证据审核的 unknown→confirmed；保存原始不确定记录。

**验收：** 晚到真实收据确认；没有收据不能转换。

**负责工作包：** T11/T21。状态：设计已收敛；实现/验证依据测试台账。

## A10｜高｜租约 hash 被误认为清理证明

**原文件：** `seed/python/src/lightsandbox_seed/leases.py`

**问题：** confirm_clean 仅检查字符串形状。作为状态机 seed 合理，直接暴露则不证明资源已清理。

**v5处置：** 确认接口仅供受信任 manager；生成实际进程/挂载/网络检查收据后释放预算。

**验收：** 用户提交形似hash不能释放配额。

**负责工作包：** T09/T38。状态：设计已收敛；实现/验证依据测试台账。

## A11｜高｜环境准备提前占用活跃配额

**原文件：** `docs/engineering/02_RUNTIME_ARCHITECTURE.md`

**问题：** 创建序列先保留全部资源再长时间构建，影响高密度与准入公平性。

**v5处置：** 构建与运行分开预算；Template准备不预占最终活跃沙盒配额。

**验收：** 慢构建不占用运行并发；准备与执行分别限流。

**负责工作包：** T09/T19。状态：设计已收敛；实现/验证依据测试台账。

## A12｜高｜暂停不等于释放内存

**原文件：** `docs/design/06_LIFECYCLE_AND_RECOVERY.md`

**问题：** 功能边界虽有说明，但产品命令缺少明确展示暂停仍收费/占预算。

**v5处置：** pause显示reservation保持；hibernate/process checkpoint不在v1。

**验收：** 暂停前后cgroup/RSS及预算实测；不可按零工作集计密度。

**负责工作包：** T21/T42。状态：设计已收敛；实现/验证依据测试台账。

## A13｜高｜Harbor 格式与 Legacy Terminal-Bench 易混

**原文件：** `docs/harbor/06_END_TO_END_ONE_CLICK_FLOW.md`

**问题：** 上轮说明出现task.yaml/根目录Dockerfile；实际Harbor入口为task.toml和environment目录。

**v5处置：** 固定Harbor原版解析模型；legacy格式显式迁移，禁止猜测结构。

**验收：** 官方样例、image-only、Dockerfile、Compose分别测试。

**负责工作包：** T33/T34。状态：设计已收敛；实现/验证依据测试台账。

## A14｜高｜Compose 能力宣告过早

**原文件：** `seed/python/src/lightsandbox_harbor/provider_core.py`

**问题：** 当前单服务seed不能证明docker_compose能力；解析Compose不实现其生命周期。

**v5处置：** 能力只有在exec/copy/stop各服务、卷、DNS和collect通过后开放。

**验收：** stop main不杀sidecar；sidecar产物后收集；保留原验证。

**负责工作包：** T20/T37。状态：设计已收敛；实现/验证依据测试台账。

## A15｜高｜Harbor 双编排引擎风险

**原文件：** `docs/harbor/06_END_TO_END_ONE_CLICK_FLOW.md`

**问题：** 自写harbor verify和upstream Trial并行维护，会产生奖励/步骤逻辑分歧。

**v5处置：** 原生provider第一；独立加载只给环境，验证委托Harbor。

**验收：** runtime包不得导入Harbor/模型SDK；单一上游验证路径。

**负责工作包：** T36/T39。状态：设计已收敛；实现/验证依据测试台账。

## A16｜高｜构建输入副作用未完全隔离

**原文件：** `docs/engineering/01_TECH_STACK.md`

**问题：** BuildKit RUN/remote frontend具有代码执行和网络能力；“无Docker”不意味着构建安全。

**v5处置：** 独立构建worker/预算；frontend digest、secret、cache作用域、entitlement政策独立。

**验收：** 无宿主Docker凭据继承；恶意context不触及管理目录。

**负责工作包：** T19。状态：设计已收敛；实现/验证依据测试台账。

## A17｜高｜OCI基础与Docker扩展混称

**原文件：** `docs/engineering/06_DOCKERFILE_TO_RUNTIME_MECHANISM.md`

**问题：** 标准config映射不能涵盖healthcheck、volume copy-up、runtime flag等所有Docker行为。

**v5处置：** image→LaunchSpec→OCI bundle，扩展由adapter逐字段映射并留测试。

**验收：** USER组解析、PATH、ENTRYPOINT清空、匿名卷初填、StopSignal差分。

**负责工作包：** T06/T20。状态：设计已收敛；实现/验证依据测试台账。

## A18｜高｜CLI 命名与 SDK 资源缺少统一合同

**原文件：** `docs/harbor/06_END_TO_END_ONE_CLICK_FLOW.md; schemas/native-objects.schema.json`

**问题：** load/create/up/reset/close的对象和阻塞语义未形成共同方法表。

**v5处置：** 资源化CLI与client，共用OpenAPI草案、方法矩阵和错误码。

**验收：** 同场景CLI/同步SDK/异步SDK请求golden一致。

**负责工作包：** T41/T42。状态：设计已收敛；实现/验证依据测试台账。

## A19｜高｜取消进程树保证过强

**原文件：** `docs/design/07_EXEC_SESSIONS_FILES.md`

**问题：** kill process group不能覆盖setsid/双重fork，不能把kill runc视为命令树结束。

**v5处置：** process/tree/sandbox三个作用域；tree未验证时明确拒绝；sandbox需要显式同意。

**验收：** 双重fork与后台服务器；检查实际cgroup空。

**负责工作包：** T11/T43。状态：设计已收敛；实现/验证依据测试台账。

## A20｜高｜OpenSandbox 兼容入口不等于字段相似

**原文件：** `docs/design/05_OPENSANDBOX_COMPATIBILITY.md`

**问题：** 原版SDK需要lifecycle与execd端点路由、headers、SSE/WS语义都吻合。

**v5处置：** 兼容facade保持独立contract与端点resolver；不强塞native字段，不改SDK判定通过。

**验收：** 固定原版SDK真实互操作；端点不能返回调用方不可达IP。

**负责工作包：** T17/T23。状态：设计已收敛；实现/验证依据测试台账。

## A21｜中｜过多选项拖慢首发

**原文件：** `docs/engineering/01_TECH_STACK.md`

**问题：** rootless/cgroupv1/FUSE/多存储路径同时推进，放大差分矩阵。

**v5处置：** 先由目标VM能力确定一个运行配置；其他路径单独列支持状态，不能静默弱化隔离。

**验收：** 目标VM实测，不靠宿主版本字符串。

**负责工作包：** T03/T07。状态：设计已收敛；实现/验证依据测试台账。

## A22｜高｜轻量收益缺少公平基线

**原文件：** `docs/acceptance/04_PERFORMANCE.md`

**问题：** 删除dockerd并不移除进程监督、DNS、日志等职责。

**v5处置：** 同镜像、同I/O、同负载比较固定开销和有效吞吐；允许结果不优于Docker。

**验收：** RSS/PSS/cgroup、FD/PID/mount、任务throughput同口径。

**负责工作包：** T05/T29。状态：设计已收敛；实现/验证依据测试台账。

## A23｜中｜新旧文档及代码状态叠加

**原文件：** `README.md; docs/design/02_ARCHITECTURE.md; docs/engineering/02_RUNTIME_ARCHITECTURE.md`

**问题：** 多版正文同时权威，规划测试容易被混写为产品验证。

**v5处置：** 旧路径迁移为导航；v5单一合同；保留历史证据但注明旧版。

**验收：** 链接、状态、schema、任务依赖自动校验。

**负责工作包：** T41/T44。状态：设计已收敛；实现/验证依据测试台账。

## A24｜高｜全量目标和扩展能力边界要独立

**原文件：** `docs/acceptance/01_PARITY_CONTRACT.md`

**问题：** “所有任务”若包含缺失硬件/特权要求，需要如实台账，不能以不支持为由从分母删除。

**v5处置：** 冻结完整manifest与宿主配置；阻塞不算通过；scope变更需新版本并保留旧目标。

**验收：** 可从逐任务证据重算分母；无隐式Docker回退。

**负责工作包：** T02/T28/T40。状态：设计已收敛；实现/验证依据测试台账。
