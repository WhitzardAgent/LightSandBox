> v5：架构/接口以docs/v5与contracts/v1为准；旧seed仅为局部逻辑参考。当前产品任务目录已更新为45项。

# 测试入口、门禁与证据

## 当前已有与后续开发

本包现在可以执行 `python3 tools/packctl.py validate`、`next`、`context`、`release-check`、`verify-manifest`，以及 `python3 -m unittest discover -s tests/pack -v`。这些只验证规划辅助工具。

下列是T00必须建立的产品开发命令合同，当前尚不存在：

```bash
./dev/check fast
./dev/check unit
./dev/check contracts --task T01
./dev/check runtime --task T10 --worker WORKER_ID
./dev/check differential --task T10 --worker WORKER_ID
./dev/check security --task T30 --worker WORKER_ID
./dev/check parity --benchmark swebench --manifest LOCKED_MANIFEST
./dev/check performance --profile LOCKED_PROFILE
```

T00实现真实的参数解析、环境检查、测试清单/数量报告和退出码。未知suite、未安装工具、没有测试、无授权worker均不能返回成功。fast/unit在本地运行，其他suite通过受控测试节点适配器执行；不把root命令权限交给自由生成的字符串。

## 证据绑定

报告至少绑定task/test IDs、候选Git提交和dirty状态、上游锁摘要、host capability报告摘要、任务manifest摘要、命令argv、退出码、时间、实际测试数量、成功/失败/阻塞数量、原始日志与SHA-256、审查者和CI job身份。源码改动后的旧测试报告不能当新证据。

协议/权限任务还要保存参考与候选请求响应、实测资源/网络行为、cleanup审计。benchmark任务保存逐任务结果JSONL、官方判据输出和固定输入摘要。敏感凭据不进入制品。

## 状态与失败

planned → in_progress → implemented → in_review → verified。任一步可转blocked或failed；修复后重新验证。verified需要已验证依赖、至少一个可校验的测试证据文件和审查记录。结构工具只做防误操作检查，可信性必须由CI/签名/分支权限实现。

不能把pytest skip、xfail、零测试或缺少Docker参考变成PASS。可以将某测试明确归类为该profile不适用，但完整目标中要求它时仍阻塞相应声明。缺少硬件/权限不要求代理伪造，也不应该诱导它放宽测试。

## 门禁自身测试

T00/T05建立“故意失败仍应红灯”用例：未知命令、错误退出码、空结果、损坏JSON、错commit、缺日志、改verifier、参考故障、过期manifest。每次修改测试runner或汇总器都重跑这些用例。

本包内单元测试也会验证部分结构门禁的失效路径。它们无法证明将来的CI完全不可篡改，相关部署保护仍需项目方实际设置。
