# 参考来源与证据状态

本包根据公开一手文档和已提供的上一版规划重新设计。`[Sxx]`在各文档中指向下列来源。架构、任务分解和门禁为本项目提出的设计，不冒充来源的已实现保证。

网页当前内容不等于不可变源码锁，不等于在用户机器运行过。S01–S18用于本轮规划核对；S19–S20为正式实现时必须核对的规范入口，本轮未逐项审查全文。`specs/upstream-lock.json`尚未锁定；开发阶段需下载/固定具体源码、SDK和制品。

### S01 · OpenAI: Harness engineering

来源：https://openai.com/index/harness-engineering/

发布日期：2026-02-11。观察日期：2026-09-23。用途：短入口、仓库内知识、可检查的架构边界；不照搬其宽松合并策略。

### S02 · OpenAI: Run long horizon tasks with Codex

来源：https://developers.openai.com/blog/run-long-horizon-tasks-with-codex

观察日期：2026-09-23。用途：区分目标、执行计划、操作规程与实际进度。

### S03 · OpenAI: Using PLANS.md

来源：https://developers.openai.com/cookbook/articles/codex_exec_plans

观察日期：2026-09-23。用途：执行计划应可独立阅读，包含验证步骤与决策记录。

### S04 · OpenAI: AGENTS.md guidance

来源：https://developers.openai.com/codex/guides/agents-md

观察日期：2026-09-23。用途：使用仓库指导与目录级上下文；实际优先级以所用客户端文档为准。

### S05 · Anthropic: Effective harnesses for long-running agents

来源：https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents

发布日期：2025-11-26。观察日期：2026-09-23。用途：区分初始化与增量开发；跨会话留下特性台账、测试结果和交接。

### S06 · OpenSandbox: repository

来源：https://github.com/opensandbox-group/OpenSandbox

观察日期：2026-09-23。用途：核对产品层次、SDK与源码位置。

### S07 · OpenSandbox: lifecycle OpenAPI

来源：https://raw.githubusercontent.com/opensandbox-group/OpenSandbox/main/specs/sandbox-lifecycle.yml

观察日期：2026-09-23。用途：生命周期兼容性的观察来源；浏览到的 main 未作为不可变锁定版本。

### S08 · OpenSandbox: execd OpenAPI

来源：https://raw.githubusercontent.com/opensandbox-group/OpenSandbox/main/specs/execd-api.yaml

观察日期：2026-09-23。用途：命令、会话、文件与流式协议的观察来源。

### S09 · OpenSandbox: execd architecture

来源：https://open-sandbox.ai/architecture/data-plane/execd

观察日期：2026-09-23。用途：了解现有进程内执行服务和监督职责。

### S10 · OCI runc

来源：https://github.com/opencontainers/runc

观察日期：2026-09-23。用途：低层 OCI 容器生命周期原语。

### S11 · runc terminal ownership

来源：https://raw.githubusercontent.com/opencontainers/runc/main/docs/terminals.md

观察日期：2026-09-23。用途：终端主端文件描述符、console socket及调用者责任。

### S12 · Docker OverlayFS driver

来源：https://docs.docker.com/engine/storage/drivers/overlayfs-driver/

观察日期：2026-09-23。用途：Docker已具备层复用与页缓存共享，必须使用公平基线。

### S13 · Linux cgroup v2 documentation

来源：https://docs.kernel.org/admin-guide/cgroup-v2.html

观察日期：2026-09-23。用途：控制器、资源归属和冻结机制；不能只检查目录存在。

### S14 · BuildKit rootless documentation

来源：https://github.com/moby/buildkit/blob/master/docs/rootless.md

观察日期：2026-09-23。用途：构建器、文件系统驱动和宿主权限仍有前置条件。

### S15 · SWE-bench official harness

来源：https://www.swebench.com/SWE-bench/reference/harness/

观察日期：2026-09-23。用途：官方gold patch校验流程与评测入口。

### S16 · CyberGym official README

来源：https://raw.githubusercontent.com/sunblaze-ucb/cybergym/main/README.md

观察日期：2026-09-23。用途：代理环境、提交服务、验证runner以及数据准备。

### S17 · Harbor task format

来源：https://www.harborframework.com/docs/tasks

观察日期：2026-09-23。用途：任务环境、solution、共享/独立验证环境和sidecar产物。

### S18 · Terminal-Bench official repository

来源：https://github.com/harbor-framework/terminal-bench

观察日期：2026-09-23。用途：发布与任务资产的核对入口。

### S19 · OCI Runtime Specification

来源：https://github.com/opencontainers/runtime-spec

观察日期：2026-09-23。用途：应锁定到实际使用的版本，并用运行时测试验证。

### S20 · OCI Image Specification

来源：https://github.com/opencontainers/image-spec

观察日期：2026-09-23。用途：镜像配置、层、manifest与平台信息；开发阶段按锁定源码核对。

## 引用与许可证

本包采用概述和设计建议，没有复制完整上游文章或代码。后续若vendor上游spec、helper或测试，保留各自许可证和版权说明。不得把本包工作名当作现成PyPI/Crates.io包名可用性的承诺。
