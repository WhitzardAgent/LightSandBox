# 原生 v1 领域模型、身份与状态

本合同是LightSandbox自定义协议。OpenSandbox及Harbor字段只能在适配层转换，不能作为同名native字段隐式混用。

## 资源与生命周期

| 资源 | 作用 | 身份/终止 |
|---|---|---|
| Image | digest固定的OCI镜像和平台 | 不可变，GC由引用控制 |
| Artifact | 构建上下文、传文件与输出制品 | digest＋owner授权；不是宿主路径 |
| Template | 已准备、镜像digest固定的环境配方 | 不可变；可被多次实例化 |
| Sandbox | 一组OCI运行资源 | `sandbox_id + generation`，删除后不复用ID |
| Environment | 一个或多个services和其卷/网络 | 不含验证器或奖励；每service映射SandboxRef |
| Execution | 一次命令的持续身份 | `execution_id`绑定sandbox/generation；断线仍是同次执行 |
| Operation | 创建/准备/删除等异步管理操作 | `operation_id`，带状态和最终结果或错误 |
| Session | 持久shell或PTY | 单独创建/attach；不等价于每次exec新shell |
| Lease | 环境占用和使用权 | ACTIVE→DRAINING→RELEASED；清理前仍计费/计预算 |
| Cleanup | 实际资源清理记录 | pending/running/complete/blocked；不能由用户传hash确认 |

API或manager重启不会改变存活对象的generation。新实例使用新ID；generation用于实例代次/fencing，revision用于状态CAS，二者不混用。名字不是安全身份、PID不是稳定资源ID。

## 命令与退出

`ExecSpec.command`为严格联合：

```json
{"command":{"kind":"argv","argv":["printf","%s","a b;$HOME"]},"timeout_ms":30000,"env":{}}
```

```json
{"command":{"kind":"shell","executable":"/bin/bash","script":"cd /app && pytest -q"},"timeout_ms":30000,"env":{}}
```

argv不经过shell、不拆词、不变量替换。shell必须明确guest中存在的绝对解释器路径。默认使用`-c`，不能偷偷变为login shell。Harbor main/sidecar所需shell规则由固定provider编译。用户指定cwd为guest路径；解析guest用户/组也在正确rootfs语义内完成。

原生数量单位：`timeout_ms / wait_timeout_ms / ttl_ms`为正整数毫秒；内存/存储为整数bytes；CPU为整数`cpu_millis`；PID为整数。JSON bool不能充作数字；NaN/Infinity拒绝。Harbor的秒与MB在adapter按固定上游定义转换，不能假定“MB”总是MiB，固定源码测试决定转换。

Execution状态：accepted→starting→running→exited，另有start-failed/unknown。`exited`表示监督通道已确认退出，退出码可以非零。状态不表达“模型是否完成任务”。退出记录为互斥联合：`exited(exit_code)`、`signaled(signal)`、`unknown`，timeout/cancel/OOM作为独立termination原因与观察证据。

Operation状态：pending→running→succeeded/failed/unknown。unknown禁止重新执行；后来获得权威收据可通过审计化reconcile转为确认态。旧unknown记录不能抹除。创建成功必须带实际SandboxRef或TemplateRef；删除成功必须带CleanupRef。

## 幂等与超时

幂等键在`principal + API version + method + canonical route + resource identity + key`范围内唯一。同键同请求返回原对象，不同请求返回409。同键冲突绝不能通过换键自动重试。请求digest基于规范化body，不含秘密明文日志。

传输超时、调用方等待超时、服务端执行预算三个不同维度。客户端等待超时不取消guest；取消明确传scope=process/tree/sandbox。tree能力未实证时拒绝，不能升级成杀整个sandbox。取消已接受不是取消已完成。

## 启动与安全正交

`launch_mode=image`保持原镜像ENTRYPOINT/CMD及PID1生命周期；native override中null/缺失表示继承，[]表示清空。Docker CLI的`--entrypoint`清CMD细则、Compose command等由各适配器先编译，不混入通用resolver。

`launch_mode=idle-agent`以ls-guest init常驻，披露被覆盖的镜像入口。不要用此模式运行数据库sidecar。`security_profile`独立决定设备、capability、seccomp、网路；启动模式不能自动放宽权限。[S03]

## EnvironmentSpec 边界

核心只接受services/networks/volumes/labels。每service包括image、launch、资源、mount和网络等物理参数。Harbor的agent_service选择、artifacts、verifier、steps、reward在HarborRecipe，不能进入runtime schema。

离线参考解析器现在可验证一个或多个service声明，但只支持无额外卷/网络/依赖的严格子集；这不构成多服务运行能力。支持字段见代码与schema；非空高级字段报不支持。多服务生产IR由T20/T35实现，不把只读解析成功当环境可执行。
