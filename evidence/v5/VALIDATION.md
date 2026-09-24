# LightSandbox v5 实际验证记录

日期2026-09-24。环境为本轮文件编制容器，不是用户VM。验证来自实际命令输出；未实施独立代码评审。

## 测试结果

native客户端/合同/CLI：**75项通过**。旧Python参考逻辑：**109项通过**。规划工具：**34项通过**。合计**218项**，均为离线或模拟传输层的逻辑测试。

| 检查 | 实际退出码 | 测试数 | 日志 |
|---|---:|---:|---|
| `native-tests` | 0 | 75 | `evidence/v5/native-tests.log` |
| `seed-tests` | 0 | 109 | `evidence/v5/seed-tests.log` |
| `pack-tests` | 0 | 34 | `evidence/v5/pack-tests.log` |
| `structure` | 0 | — | `evidence/v5/structure.log` |
| `next-task` | 0 | — | `evidence/v5/next-task.log` |
| `release-blocked` | 1 | — | `evidence/v5/release-blocked.log` |
| `cli-version` | 0 | — | `evidence/v5/cli-version.log` |
| `cli-env-plan` | 0 | — | `evidence/v5/cli-env-plan.log` |
| `cli-exec-dry-run` | 0 | — | `evidence/v5/cli-exec-dry-run.log` |

`release-blocked`退出1是预期结果：未锁来源、未探测用户host、产品任务未验证；没有把发行阻断算成产品通过。CLI提交dry-run返回executed=false，env plan返回execution_ready=false。

## 可复核的实现状态

11条native HTTP请求路径都有MockTransport请求/schema/头部交叉检查；不存在服务端实现。同步/异步wait、参数联合、generation、错误、取消等待、关闭连接等为本轮参考代码验收范围。schema、路由和状态文件之间经过交叉测试，但未经过实际HTTP server或OpenAPI工具全兼容认证。

源码语法检查：42个Python文件、32个JSON文件、6个TOML文件、1个YAML文件可解析（计数不包括随后写入的本报告JSON与最终manifest）。文档内部链接、任务依赖与需求/测试映射通过packctl检查。

早期审计测试先暴露了falsey输入被归一化、environment查询未带generation、无效error结构和预览env泄漏，随后修复并重新运行。早期红灯日志保留；其中初始导入错误只证明代码不存在，不宣称等同全部行为红灯。

旧规划用例`test_missing_design_reference_is_rejected`原先删除已退役的v4跳转文档，本轮改为删除当前T14实际引用的规范，负向断言保持不变。此项修改跟随文档迁移，没有放宽运行时/benchmark测试。

## 未验证事项

当前没有cargo/rustc/runc/docker/buildctl；存在Go工具但本轮未编译Go组件。Rust seed未编译；没有真实sandbox、BuildKit构建、Harbor导入/运行、OpenSandbox互操作或三个完整基准。用户VM能力与性能未测试；无KVM和无需换内核是设计目标，不是对任意host的兼容证明。

精确源码/SDK/镜像/依赖锁仍待真实环境完成。本轮审计解决的是规范定义冲突，运行时风险仍由各任务实测关闭。

## 归档检查

最终归档包含全包文件和逐文件SHA-256 manifest。ZIP构建后再解压复核文件列表、摘要、packctl与reference测试。归档验证完成后才对用户提供下载链接。
