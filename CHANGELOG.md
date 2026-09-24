# v5 修订摘要

- 原生EnvironmentSpec移除verifier/reward/agent；Harbor保留原Trial和Verifier。
- CLI统一lsbx；exec提交与execution管理分组，SDK同步/异步共用请求合同。
- 新增native OpenAPI/JSON schema/状态机/surface和75项本轮native reference测试。
- 首发每sandbox轻量supervisor；撤回“无shim职责即可低开销”的隐含假设。
- 命令argv/shell、空值、数量单位、generation/revision、Operation/Execution退出语义统一。
- 支持晚到权威证据确认UNKNOWN；不进行自动副作用重放。
- plan/prepare/create分开，构建预算与运行预算分开，训练destroy/recreate不依赖snapshot。
- 新增T41–T44；旧规范变为迁移导航，历史文本不进入实施上下文。
- 新增可执行客户端和CLI参考，不提供假运行时或伪一键加载。
