# HTTP / OpenAPI 与兼容门面

`contracts/v1/native.openapi.json`是原生v1草案，带`x-implementation-status`。它不是OpenSandbox规范。配套JSON schema为严格离线参考子集，生产高级字段须在同一版本化来源中补齐，不能在Rust/Python各自偷偷放宽。

## 原生路由

| 路由 | 响应 | 语义 |
|---|---|---|
| POST /v1/sandboxes | 202 Operation | 已接受创建，不承诺READY |
| GET /v1/sandboxes/{id} | 200 SandboxInfo | 显式generation校验 |
| DELETE /v1/sandboxes/{id} | 202 Operation | 清理流程，不是立即回收 |
| POST /v1/sandboxes/{id}/executions | 202 Execution | 稳定exec identity，不重复执行 |
| GET /v1/executions/{id} | 200 ExecutionInfo | 真实状态与退出记录 |
| POST /v1/executions/{id}:cancel | 202 Operation | 取消已接受，scope明确 |
| GET /v1/operations/{id} | 200 Operation | 管理操作及结果 |
| POST /v1/templates:prepare | 202 Operation | 上传/镜像/构建准备完才返回Template |
| POST /v1/environments | 202 Operation | 从不可变Template实例化 |
| GET /v1/environments/{id} | 200 EnvironmentInfo | service状态及引用 |
| DELETE /v1/environments/{id} | 202 Operation | 回收组资源，generation必填 |

以上HTTP客户端请求已实现，server尚未实现。文件、流、分页、sessions、endpoints、lease等完整产品路由见surface台账，状态planned。

写操作要求`Idempotency-Key`。针对活跃sandbox/environment的读写要求`X-LS-Generation`；Execution ID本身不可复用，服务端保存其sandbox代次绑定。访问控制依据认证principal，不能信任body中传入的owner。

默认创建是202，与OpenSandbox锁定版本的create行为可以不同。兼容facade负责按其协议等待或返回状态，native域不因此改变状态机。[S06/S07]

## 统一错误

```json
{"error":{"code":"STALE_GENERATION","message":"resource incarnation changed","request_id":"rq_123"}}
```

目标映射：400 INVALID_ARGUMENT；401 AUTHENTICATION_REQUIRED；403 FORBIDDEN；404 NOT_FOUND；409 CONFLICT/STALE_GENERATION；412 CAPABILITY_UNAVAILABLE/POLICY_UNSUPPORTED；429 RESOURCE_EXHAUSTED；500 INTERNAL_FAILURE；503暂不可用。错误不能统一200。成功HTTP连接关闭不能作为guest命令成功。

## 输出与重放（生产设计，未实现）

执行输出保留raw bytes。事件含execution_id、sandbox_generation、sequence、stream、byte offset和payload；stdout/stderr各自字节顺序，PTY合流。终止事件含真实exit receipt和final offsets。

订阅cursor属于特定execution。已GC范围返回STREAM_GAP与现存范围，不自动重新执行。在线队列有界；慢消费者可以改取输出Artifact，不能让guest写阻塞无限占用内存，也不能把丢日志当完整输出。输出artifact和exit receipt是两个独立对象，缺其一如实报告。

SDK输出decoder维护跨chunk编码状态；原始字节不能在核心先替换成UTF-8。Harbor需要文本时，由适配层采用固定上游策略转换。大输出单独传输，SQLite只记索引。

## OpenSandbox facade

固定lifecycle、execd、SDK制品以及引用服务的版本/hash。兼容表要同时列出：spec声明、SDK使用、参考实际行为、候选实现状态。SSE/WS、capabilities、endpoint、headers、TTL、pause/snapshot逐项测试。[S06/S07]

每个sandbox的execd兼容地址可由中心受认证代理虚拟路由，无须在sandbox内运行HTTP daemon；但必须满足SDK的连接方式，不能要求用户修改SDK代码。入口token只授予特定sandbox/端口/用途，禁止管理token随重定向或代理转发泄露。

原版SDK不携带native generation时，facade通过其外部ID映射到不可复用的原生身份；不能把最新代自动接给旧ID。未知功能明确返回上游约定错误，不伪造capabilities。
