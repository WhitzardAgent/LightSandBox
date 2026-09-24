> v5：架构/接口以docs/v5与contracts/v1为准；旧seed仅为局部逻辑参考。当前产品任务目录已更新为45项。

# 独立发行核验指令

执行T32。先核对候选release commit、源/SDK/镜像/数据集锁、HostReport和完整任务manifest。不要复用不同版本的绿灯报告。

运行完整登记门禁，独立重算三类基准完整分母、环境覆盖、正向oracle覆盖和结果一致性。分别确认Cyber代理与verification runner无Docker依赖。任何missing asset、oracle gap、reference错误和未解决候选差异都必须保留，不能从分母去除。

检查原版OpenSandbox SDK互操作、安全/故障/资源泄漏、同条件性能SLO、干净VM安装升级与制品哈希。仅有implemented、文档检查或自报总结不足以发行。

最终报告明确已验证的API版本、任务发布、宿主能力组合和性能条件。未满足时输出阻塞列表与责任任务；不输出未经支持的100%或任意内核兼容声明。
