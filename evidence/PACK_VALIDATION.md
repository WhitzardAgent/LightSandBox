> 此文件保留v2历史记录。v3本轮状态见[V3_VALIDATION](V3_VALIDATION.md)。

# 本次交付的实际验证记录

日期：2026-09-23。验证对象为规划文档与Python辅助工具，**不包含任何沙盒产品、Docker/OpenSandbox差分或三个基准的运行结果**。

## 实际命令与结果

| 命令/检查 | 实际结果 |
|---|---|
| `python3 -m unittest discover -s tests/pack -v` | 34项通过，0失败，0跳过；退出码0 |
| `python3 tools/packctl.py validate` | 任务/依赖/需求/测试/功能映射、JSON和Markdown链接检查通过；退出码0 |
| `python3 tools/packctl.py next` | 初始只有T00可进入执行 |
| `python3 tools/packctl.py context T14` | 返回该任务的读取列表和4条具体用例，未加载全部文档 |
| `python3 tools/doctor_readonly.py` | 在本工具容器完成只读观察，明确NOT_PROBED；不是用户VM能力证明 |
| `python3 tools/packctl.py release-check` | 正确拒绝发行，退出码1：产品未实现、锁/宿主/全量基准未完成 |
| Python AST与Markdown代码围栏/来源标识检查 | 语法解析与结构检查通过 |

原始测试结果见 [tool-tests.log](tool-tests.log)。结构结果见 [pack-structure.json](pack-structure.json)。预期阻断结果见 [release-preflight.json](release-preflight.json)。功能映射校验在加入实现前曾有2项失败，原始记录见 [tool-tests-red-feature.log](tool-tests-red-feature.log)，之后加入校验并完成全部回归。

## 这些检查证明与不证明的内容

已检查33个任务、24项要求、30组功能、147条待实现验收规格的关联和文档结构。辅助工具用例包括依赖循环、缺失引用、无证据verified、零测试、原始制品哈希、归档篡改、本地源码锁与只读观察。

147条是未来产品验收规格；34条才是本轮实际执行的辅助工具测试。产品状态仍全部planned。本轮没有下载完整上游源码与任务资产，没有在用户VM主动探测，没有构建LightSandbox，没有确认任何性能目标或全量任务兼容。

最终归档使用逐文件SHA-256的MANIFEST.json与ZIP CRC核验。解压后可运行 `python3 tools/packctl.py verify-manifest` 复核原始交付；正常开发改动后原始哈希不再适用。
