# 首次交给Coding Agent

你将实现独立LightSandbox，先读AGENTS.md、START_HERE.md和docs/v5/00_PRODUCT.md。运行`python3 tools/packctl.py validate`与`python3 tools/packctl.py next`，按任务读取上下文，从T00开始。

以docs/v5和contracts/v1为当前规范；history为只读来源；seed为局部逻辑参考，client/python为原生客户端参考，二者都没有daemon。不要重新实现已存在的参考逻辑，也不要把它当已完成产品。

首个真实链路用预构建OCI image→runc→exec→files→delete→实际资源回收。随后supervisor故障测试，再BuildKit/Harbor。不得把核心模型加入verifier/agent/reward字段。无需KVM，无需替换宿主内核；权限和必要能力缺失记blocked。

完成一个有独立验收边界的任务再推进依赖。每次变更先写失败测试或最小复现，保留真实证据和交接。公共协议、源锁和数据库迁移由指定集成者管理。不要修改验证器、分母或隐藏失败；不要退回Docker而算候选成功。
