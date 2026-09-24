# Harbor 接入：provider优先，保留原始任务流程

## 当前上游依据

2026-09-24读取的Harbor文档及源码显示：任务使用`task.toml`、`instruction.md`和`environment/`；Docker provider可从预构建镜像、Dockerfile或Compose获得环境。EnvironmentFactory支持导入路径的第三方环境类。BaseEnvironment为集成边界。[S10/S11/S12]

这只是当次观察，不替代实际源码commit/wheel/hash锁。Harbor持续演进，代码与网页可能不在同一revision。T33必须在完整安装中检查抽象方法、constructor、resource/network capabilities及真实方法签名。AST签名检查是漂移预警，不证明行为兼容。

## 三层边界

1. **原版Harbor**：任务配置、dataset、Trial、Agent、步骤、Oracle、Verifier、reward、artifact收集策略。
2. **LightSandbox Harbor provider**：将BaseEnvironment调用映射为service-oriented执行/传输/网络/停止；只做必要协议转换。
3. **原生Environment服务**：镜像/进程/卷/网络/身份/资源，无agent/verifier/reward含义。

EnvironmentSpec不含verifier或artifact_rules。HarborRecipe可包含如何选择环境配置、main service、验证器配置及构建输入；recipe位于adapter，不序列化为runtime核心业务类型。

## 一键目标与实现状态

未来真实验收命令：

```bash
harbor run -p ./tasks/example \
  --env lightsandbox_harbor.provider:LightSandboxEnvironment \
  -a oracle
```

本包原provider仍是受限seed，缺原生后端和完整上游互操作，不能运行以上端到端路径。原生客户端参考是v5新增工作成果，尚未连接原provider。禁止把FakeEnvironment/MockTransport通过叫作Harbor已兼容。

最终`lsbx env up --from harbor PATH`只提供环境加载便利链路；执行和评测命令仍可使用Harbor原版。不要在核心新增verify子命令。任务路径来自用户明确授权目录；后端导入路径由operator固定，禁止任务内容指定Python module执行。

## 环境编译顺序

定位任务→原版模型解析→确定agent/verifier构建上下文→列出外部环境变量/secret/remote sources并授权→按固定provider优先级选择镜像/Dockerfile/Compose→无特权compose-go规范化→逐字段覆盖检查→生成通用EnvPlan→上传合法context→BuildKit prepare→固定image digest→宿主准入→实例化。

预构建image、Dockerfile、force-build的优先级由锁定provider决定。不能凭自拟优先级丢弃已有Dockerfile。image-only环境目录的运行时上传行为也需复制原版行为。不要把tests/solution提前放进agent镜像；构建context泄露会破坏评测。[S10]

## 多服务与独立验证

只有当per-service exec/copy/stop均实现，才宣告`docker_compose=true`。Healthcheck、depends_on条件、service DNS、卷初始内容、bind路径授权及默认main服务都属于环境行为，parse成功不足以通过。[S13]

当前上游独立验证流程还涉及：agent产物及声明路径传到新的验证环境；sidecar collect hook可能需要先停main，再在sidecar执行dump；多步骤中并非每一步都停止main。严格沿用原版控制顺序，不在provider悄悄把stop main实现成destroy whole group。[S10/S11]

Sidecar证据并非天然可信：如果agent已攻破sidecar，它也可以影响文件。reference-faithful模式保持原任务语义，强化隔离实验必须单独标记，不能变更判据后叫兼容。

## 逐类方法映射

| Harbor调用类别 | Native对象/操作 | 验收 |
|---|---|---|
| start(force_build) | prepare Template + create Environment + READY | 不能把202当已启动；正确build context |
| exec(command, cwd, env, timeout, user) | main Sandbox ExecSpec.shell | 固定shell、秒/资源单位、用户和输出规则 |
| per-service exec/copy/stop | service name→SandboxRef | main与sidecar隔离，停止主服务不毁旁路证据 |
| upload/download | 客户端文件流↔Artifact↔guest helper | 主机路径不传为root bind |
| stop(delete=False/True) | 保留/完整清理的独立操作 | 不伪造自动restart；保留identity以便对账 |
| set_network_policy | 有版本的network变更Operation | 等实际生效，包含已有连接处理 |
| capabilities | adapter实现∩host实际能力∩策略许可 | 不硬编码“全部支持” |

## 训练复用

训练程序可以复用prepared Template与镜像cache，每轮创建新的Environment。Lease签发、准入、到期、drain、清理和释放单独持久化。模型等待期间不假定进程没工作，也不自动pause改变任务。reset优先销毁再创建；进程内存、数据库、DNS/TCP、volume不能只靠清空workspace恢复。

## 必须执行的验收阶梯

锁定源码接口 → 官方单服务样例 → image-only → 自定义ENTRYPOINT → Compose sidecar → 单独停止/取产物 → 独立验证环境 → 动态网络 → 多步骤 → 训练连续轮次 → 完整manifest。

参考必须在Docker、OpenSandbox、候选三方分别记录，不能假定前两者天然相同；差异使用固定脚本/patch/PoC排除模型随机性。
