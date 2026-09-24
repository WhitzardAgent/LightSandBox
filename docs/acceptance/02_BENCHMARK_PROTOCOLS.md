# 三类基准执行与验证协议

本目录的适配器只为验证独立沙盒提供测试。运行时核心不得依赖benchmark库。

## SWE-bench

先明确数据产品、split和revision，例如原始集合、Lite或Verified不能混为同一个“全量”。T02按用户目标列出完整清单和重叠关系，保留去重前后标识。T01固定官方harness与依赖。官方harness支持gold patch入口，用于环境验证。[S15]

参考执行记录镜像构建层、平台、测试脚本、patch、测试结果JSON和日志。候选适配器只替换容器create/exec/copy等传输，不改FAIL_TO_PASS、PASS_TO_PASS及官方判分逻辑。先跑gold与已知错误patch两种固定输入；错误patch不能因测试没有执行而被接受。

验收覆盖非root用户、依赖安装、网络关闭后的测试、只读制品、生成文件权限和超时。记录原始完整测试列表与实际执行数量，零测试收集是错误。是否需要构建器、哪些镜像可复用，由全量资产分析决定。

## Terminal-Bench / Harbor

锁定benchmark发布、完整任务目录集合、任务格式版本与实际harness。不要把Harbor能表达的所有平台都当成本次数据集已使用的平台，也不要凭印象断言当前集合只需要最简单Linux容器。真实的Dockerfile/Compose/task配置清单决定需求。[S17][S18]

逐任务枚举sidecar、named volume、bind类型、entrypoint、健康检查、依赖顺序、网络切换、共享或独立verifier环境、产物路径、超时、capabilities、ulimits、shm、CPU/内存/磁盘等字段。每一种观察到的字段都进入通用语义测试。未知字段提前拒绝并记录，而非删除后继续跑。

Oracle solution存在时使用官方solution与官方verifier。没有solution时标记正向证明缺口，不能用空脚本冒充oracle。两种verifier拓扑都按原任务保持：共享验证保留工作区和进程状态；独立验证严格执行原有制品传输和只读/可写声明，不能随意改变任务语义。

重点检测PTY、bash持续状态、服务器后台进程、Ctrl-C、网络服务发现，以及agent退出后验证器是否能读取正确制品。验证器自身没有运行、结果文件未生成和结果为失败是三种不同情况。

## CyberGym

分别追踪代理执行环境、submission服务与验证runner。官方流程使用Docker资产并提供不同数据准备方式；固定实际使用模式、资产版本和验证入口。[S16]

T26首先让候选提供代理工作区、repo、编译调试与受控提交路径。此时若verification仍由Docker启动，只能报告“代理执行路径兼容”，不能报告整个CyberGym执行栈已无Docker。

T27再实现官方验证runner的运行时传输适配：同样的二进制/完整环境、启动参数、PoC文件、sanitizer输出和官方成功判定。原判据代码保持不变，适配器diff独立审查。禁止把验证规则迁移成候选自写的近似检查器。

每个任务用可追踪的有效PoC或固定成功轨迹；先在参考环境验证其仍有效。没有已知成功样本的任务保留缺口，不假定公开资产包含全部成功PoC。固定无效PoC和格式错误输入作为负控，检查虚假成功和错误归因。

重点覆盖GDB/ptrace、ASAN/UBSAN相关内存限制、长时间编译、进程崩溃、core/log传输、大文件、网络提交超时与重复提交语义。特权能力仅在任务声明范围内启用。submission端口不可暴露给无关外部网络。

## 渐进执行，但全量目标不变

先用字段覆盖导向的canary集缩短反馈，再对完整manifest执行。canary选择由T02字段覆盖产生并保留选择理由，不凭容易通过选样。完整测试可以分批和续跑，但每次结果绑定候选commit、参考锁和manifest；修复后重跑受影响部分并执行预定义全量发布回归。
