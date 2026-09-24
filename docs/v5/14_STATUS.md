# 实现状态与限制

## 已存在且本轮重新测试

- `seed/python`：旧版受限Harbor读取、文件bundle、journal、events、leases、bridge和provider参考；109项历史逻辑测试。
- `tests/pack`：34项规划工具测试。
- `client/python`：新增native v1 Python类型、同步/异步HTTP客户端、CLI请求编译与离线环境计划；新增测试见`evidence/v5/`。

## 没有实现或尚未证明

Rust daemon/runc资源执行；按沙盒supervisor；真实PTY/命令树取消；BuildKit完整构建；Compose服务网络和卷；原版Harbor/OpenSandbox互操作；完整三基准；生产响应体/流预算、严格同步端到端超时；独立安全审计；实际VM兼容与密度收益。

本机无cargo/rustc/runc/Docker时，不为它们写绿色结果。已有Rust seed只作为源代码，不是编译通过声明。新HTTP客户端需要真实兼容服务；`--dry-run`不产生沙盒。本包不自动安装宿主软件、不改内核/网络。

## 出版物与产品版本

v5是交付包迭代号；native API版本v1是设计合同号；Python参考版本0.5.0.dev0；它们不表示成熟产品v1已经完成。`specs/upstream-lock.json`等真实锁定工作仍明确未完成。网页观察及本地库版本不冒充固定源码/二进制锁。

本轮最终验证的实际数量、命令和限制统一记录在`evidence/v5/VALIDATION.md`。旧验证记录为历史材料，不能当本轮的证据。
