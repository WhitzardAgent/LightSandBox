# CLI：统一命名、阻塞语义和错误输出

## 产品命令体系

可执行名称统一`lsbx`，包名LightSandbox。全局参数放在子命令前；guest argv放在`--`之后。`--endpoint`来自显式参数→`LIGHTSANDBOX_ENDPOINT`→本地UDS。令牌来自环境/受保护配置，避免命令行明文token。CLI默认不读取用户shell环境作为sandbox env。

| 命令 | 目标语义 | 本包状态 |
|---|---|---|
| `lsbx version` | 客户端/协议/服务能力分别显示 | 已实现客户端参考 |
| `lsbx env plan --file env.json` | 只读检查native环境定义，不构建 | 已实现严格子集 |
| `lsbx exec --generation N ID -- argv...` | 提交一次执行，返回ExecutionRef | 已实现HTTP提交；服务未实现 |
| `lsbx exec ... --shell SCRIPT --shell-executable /bin/sh` | 显式guest shell | 已实现请求编译 |
| `lsbx sandbox create --image REF` | 创建请求，返回Operation | 已实现HTTP提交；需服务 |
| `lsbx sandbox get ID --generation N` | 查询同代对象 | 已实现HTTP请求 |
| `lsbx sandbox delete ID --generation N` | 返回清理Operation，不把202当已删 | 已实现HTTP请求 |
| `lsbx op get/wait ID` | 查询/等待管理操作 | 已实现客户端轮询 |
| `lsbx env up --from harbor PATH` | 编译→上传→prepare→create→等待就绪 | 产品目标，未实现 |
| `lsbx image import/build/pull` | 受控资产管理与构建 | 产品目标，未实现 |
| `lsbx env get/delete` | 管理多服务组 | SDK有请求方法；CLI未实现 |
| `lsbx files upload/download` | 传输内容，不传任意宿主bind路径 | 产品目标，未实现 |
| `lsbx execution get/wait/logs/cancel` | 查询结果/重放输出/明确取消 | 产品目标，未实现 |
| `lsbx session open/attach/resize/close` | 明确持久会话 | 产品目标，未实现 |
| `lsbx lease acquire/renew/release` | 训练占用与清理 | 产品目标，未实现 |
| `lsbx doctor / gc plan / gc apply` | 主动探测与可审计清理 | doctor旧只读工具可用；生产CLI未实现 |

**不提供核心`harbor verify`。** 验证委托`harbor run`及其原始Verifier。旧`lightsandbox harbor load/verify/reset`示例已经撤回，避免重复Trial引擎。

## 当前可运行示例

```bash
python3 tools/lsbx.py --json version
python3 tools/lsbx.py --json env plan --file examples/native/environment.json
python3 tools/lsbx.py --json exec --generation 1 --dry-run sb_demo -- printf '%s' 'a b;$HOME'
python3 tools/lsbx.py --json exec --generation 1 --dry-run --shell 'cd /app && pytest -q' --shell-executable /bin/bash sb_demo
```

dry-run只返回本地请求计划，包含`executed:false`。它不会执行runc、Docker、构建、联网或修改宿主。env plan中的`execution_ready:false`即使镜像已经是digest也保留，直到真实prepare/host检查完成。

非dry-run管理命令会尝试连接真实daemon。不存在服务时返回错误；不会启动替身服务器或回退Docker。CLI默认本地UDS，异地只能HTTPS；非loopback明文HTTP拒绝。

## 阻塞策略

当前create/exec/delete是提交型命令，收到202只输出Operation/Execution。当前版本的`op wait`只等待管理Operation，不用于执行流。最终产品增加显式`--wait`，仍分开输出“已提交”和“已完成”。`env up`是显式便利命令，默认等READY，其分步operation IDs需保留，部分失败返回可恢复记录。

每次写操作可传`--idempotency-key`；CLI未传时在发送前生成。响应不确定时必须把key写到错误JSON，方便查原请求。脚本重试应复用原key，不能把相同命令当新请求。

## 输出、退出码与交互

当前命令的人类模式使用缩进JSON，`--json`输出单个机器可读对象。成功数据到stdout，诊断/错误到stderr；JSON不含ANSI和交互提示。脚本必须同时检查退出码和响应状态。

当前参考退出码：0=命令完成/请求已接受（需看对象状态）；2=参数/本地计划错误；8=客户端等待到期；10=请求或结果不确定；11=传输/HTTP/协议错误。**当前exec只提交，所以0不表示guest返回0。**

产品目标的execution wait模式：默认guest非零转CLI 1；可选`--propagate-exit`转发guest原始码。传输/平台错误仍在JSON里标明错误层，不能通过唯一数字区分所有可能冲突。信号与Ctrl-C处理：先停止本地attach，是否发guest信号由明确选项决定；不可把退出CLI自动升级为销毁整个environment。

## 易错行为必须测

shell元字符作为argv保持原字节；空参数不丢；--env重复键拒绝；global flags与guest flags分离；--json错误可解析；超时不重发；陌生命令非零退出；没有generation不能修改对象；URI不允许嵌入用户名密码或跨源重定向；env值在exec dry-run中全部遮蔽。命令脚本/argv本身仍会显示，调用方不得在argv或shell里嵌入秘密；生产secret引用由单独能力支持。

最终CLI完整清单由`contracts/v1/surface.json`维护，实施状态必须与实际可执行子命令一致。未实现命令不添加会返回成功的空壳。
