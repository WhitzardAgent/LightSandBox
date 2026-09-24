# Harbor专项开发入口

先读docs/v5/07_HARBOR.md、docs/v5/06_HTTP.md及T33–T40中当前可执行任务。Harbor后端固定为`lightsandbox_harbor.provider:LightSandboxEnvironment`，使用官方自定义环境导入机制，必须用锁定的真实Harbor测试。

Harbor决定Trial/Agent/Verifier/Reward，适配层只管理执行环境与操作；核心EnvironmentSpec仅有service/network/volume。不得新增核心harbor verify。先单服务真实链路，再Compose、独立验证、动态网络、产物传递。

seed provider还使用旧开发协议，必须迁移到native客户端并完成真实测试；factory注入与AST形状检查都只是开发辅助，不能做兼容声明。注册表下载和联网拉取需显式许可。
