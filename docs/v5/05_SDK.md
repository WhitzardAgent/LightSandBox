# Python SDK：同步与异步共用合同

## 安装单元

`lightsandbox`为原生客户端导入名；当前发行名为`lightsandbox-client-reference`，避免把未完成系统发布成正式daemon。SDK只依赖HTTPX，Harbor为独立可选集成包。最终生产Python SDK可使用Pydantic增强模型，但不另建一套字段定义；结构以native合同为准。

HTTP同步/异步共享请求构造、路径、鉴权和错误映射。禁止用`asyncio.run()`包装同步接口，否则在已有事件循环中出错。当前`Client`使用HTTPX同步传输，`AsyncClient`使用异步传输。[S09]

## 已实现的调用（需真实兼容服务）

```python
from lightsandbox import Client, SandboxRef, ExecSpec

with Client('unix:///run/lightsandbox/api.sock') as client:
    op = client.sandboxes.create(
        {'image': 'your-registry/image@sha256:<real-digest>', 'launch_mode': 'image'},
        idempotency_key='create-request-001',
    )
    done = client.operations.wait(op['id'], timeout_ms=120_000)
    ref = SandboxRef(**done['result'])
    execution = client.executions.start(
        ref, ExecSpec(argv=['python3', '-V'], timeout_ms=10_000),
        idempotency_key='exec-request-001',
    )
    # 提交返回execution id，后续查询才知道真实退出状态。
    current = client.executions.get(execution['id'])
```

示例中的image digest是占位解释，不是可拉取镜像。本包不提供daemon，以上只演示当前HTTP客户端方法，不声称已经完成运行。

```python
from lightsandbox import AsyncClient, SandboxRef, ExecSpec

async def submit(client: AsyncClient, ref: SandboxRef):
    return await client.executions.start(
        ref, ExecSpec(shell='cd /app && pytest -q', shell_executable='/bin/bash'),
        idempotency_key='exec-request-002',
    )
```

资源句柄由调用方显式保留；关闭client仅关闭连接，不销毁远端对象。`sandboxes.delete`返回Operation，其完成才说明已清理。future lease上下文管理器可在退出时release，但名字和文档必须明确副作用，清理失败不能掩盖任务原始异常。

## 实现状态

已实现客户端：sandboxes.create/get/delete；executions.start/get/cancel；operations.get/wait；environments.prepare/create/get/delete。支持UDS、HTTPS、loopback HTTP；关闭隐式代理继承和重定向；POST/DELETE等变更请求传输中断不自动重试。

这些方法目前返回native JSON dict，输入ExecSpec/SandboxRef有严格类型验证。完整返回模型、分页、流式输出、文件传输、会话、上传重试、服务能力协商仍为产品工作包。不要把参考客户端称作稳定生产SDK。

## 错误语义

- `ContractError`/ValueError：未发送的本地输入错误。
- `ApiError`：服务返回的错误或查询连接失败，含稳定code/request_id/status_code。
- `AmbiguousRequest`：写请求的结果不确定，包含原幂等key；不能自动换key重试。
- `OperationUnknown`：服务记录不确定，等待进一步对账。
- `WaitTimeout`：调用方停止等待，操作仍可能运行。

SDK默认不把guest非零退出变成网络异常。最终提供`.check_returncode()`作为显式便利方法；没有退出收据或已断流时不得生成return_code=0。

## 时限、并发和协议边界

原生exec预算是服务端timeout_ms，HTTP timeout限制传输阶段，operations.wait限制调用方等待。异步wait用绝对deadline和asyncio cancellation；同步参考实现为轮询deadline＋每轮HTTP阶段timeout，**尚未提供恶意慢速分块响应下严格端到端时限/响应体内存限额**，列为T42的生产化验收项。

AsyncClient应按应用生命周期复用连接池，一般不为每次命令新建client；并发上限由调用方/服务端准入共同控制。取消等待不会自动执行executions.cancel。secret、输出和动作正文不写默认控制日志。

## 独立加载Harbor环境的目标SDK

未来可选集成包提供`HarborSource.plan(path)`及`prepare/create`便利流程；其返回通用Template/EnvironmentRef。数据集registry解析、远程下载和格式迁移由Harbor或明确调用方处理。离线读取与联网获取分开，自动环境变量展开需要可审阅映射。

训练以不可变Template实例化新Environment，旧身份失效。不会把`.reset()`定义成单纯删除工作区。共享验证/独立验证仍委托Harbor，SDK核心没有`.verify()`。

## 易用层与底层分离

产品便利层计划提供create_and_wait与executions.run，但底层create/start始终返回稳定Operation/Execution身份。便利层超时异常仍携带该身份，不销毁对象。同步和异步均采用相同字段；不采用靠运行时判断来混用await的函数。

Environment查询方法当前为`client.environments.get(environment_id, generation=N)`，与修改接口一样校验代次。客户端关闭只释放连接；最终Environment/Lease上下文管理器只有在名字明确为ephemeral/lease时才允许自动回收，cleanup错误不能掩盖任务原异常。

公开参数错误由ContractError报告；来自旧seed的BackendFactory不作为SDK扩展入口。原生客户端可被Harbor provider使用，必须完成适配及真实互操作测试后才能宣称接通。
