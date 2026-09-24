# v4 → v5 迁移

## 开发代理从何开始

先读v5权威文档；旧design/engineering/harbor正文已变成迁移导航。原始v4压缩包仍可作为历史基线，不需要把两版正文同时塞入上下文。

## 逐项迁移

| 旧用法/概念 | v5 |
|---|---|
| EnvironmentSpec.verifier/artifact_rules | 移至HarborRecipe/原版Trial；runtime拒绝 |
| command:str、timeout_sec混用 | ExecSpec.command union；native timeout_ms |
| init默认用于所有服务 | launch_mode=image默认；idle-agent显式 |
| docker-parity/cyber等混合profile | launch_mode与security_profile分开 |
| shared worker broker默认 | per-sandbox supervisor先保证恢复，后测共享 |
| close可能销毁sandbox | Client.close仅连接；delete/release显式副作用 |
| daemon restart增加generation | 存活对象不变；replacement新ID |
| UNKNOWN不可恢复/直接重试 | 证据确认可收敛；禁止自动重执行 |
| harbor verify/core reward | 委托原始Harbor；核心不持有判据 |
| 一个create里慢构建＋运行配额 | plan→prepare template→instantiate |
| 各处手写API示例 | contracts/v1 + method surface + golden测试 |

## 参考代码迁移约束

`seed/python`与`seed/rust`保留旧版已测逻辑供复用，但它们的handle/event/command/journal不被视作native v1 API。禁止让生产server直接导出旧schema。`client/python`是v5原生SDK/CLI参考，真实provider适配由T36/T42完成后再连接。

旧provider仍只声明单服务能力，没有native transport；后端factory配置只是测试/开发注入机制，不可从任务metadata读取。生产通过operator控制的连接配置和固定包入口建立客户端，不能把任意Python导入路径暴露成任务功能。

对已有实际实现仓库，先生成接口/DB/CLI diff与迁移测试，不强制删除用户代码。状态迁移必须备份并能回滚；已存在sandbox的旧ID不应自动指向新实例。
