> v5：架构/接口以docs/v5与contracts/v1为准；旧seed仅为局部逻辑参考。当前产品任务目录已更新为45项。

# 后续会话继续指令

继续LightSandbox，保持AGENTS的目标和安全边界。先读取当前Git差异、state/project-status.json与最近交接，运行packctl validate和next。选择一个依赖已verified的任务，运行context并只加载其必要设计/代码/测试。

核对上一会话证据是否绑定当前commit，不依赖“完成”文字。若有未提交用户修改，保护它们，不重置或覆盖。每次只处理当前任务，使用失败测试→最小实现→真实验证→独立review的循环。

缺少工具、权限、source lock、任务资产或测试worker时按事实标BLOCKED，不能静默skip或使用mock当真实验收。不改变测试分母、官方verifier或安全profile来获得通过。

按任务卡保存原始证据和handoff。只有集成者根据可信CI与不同责任人的review更新verified。当前卡通过且授权范围允许时可继续下一个ready任务；否则明确停点和最小下一步。
