# v3来源与验证边界

观察日期：2026-09-23。下列均为官方文档/原始代码。网页缓存、main/master和latest链接是观察依据，不构成不可变源码锁；本轮无法在工作容器获取完整源码checkout，因此没有伪造commit或依赖锁。设计、性能目标和风险处理属于本项目工程决策，不是上游已实现能力的转述。

- **S01** [Harbor自定义环境工厂](https://raw.githubusercontent.com/harbor-framework/harbor/main/src/harbor/environments/factory.py)
- **S02** [Harbor BaseEnvironment与服务级接口](https://raw.githubusercontent.com/harbor-framework/harbor/main/src/harbor/environments/base.py)
- **S03** [Harbor环境定义与镜像优先级](https://raw.githubusercontent.com/harbor-framework/harbor/main/src/harbor/environments/definition.py)
- **S04** [Harbor任务结构与独立验证](https://www.harborframework.com/docs/tasks)
- **S05** [Harbor CLI自定义--env](https://raw.githubusercontent.com/harbor-framework/harbor/main/src/harbor/cli/jobs.py)
- **S06** [Harbor项目元数据](https://raw.githubusercontent.com/harbor-framework/harbor/main/pyproject.toml)
- **S07** [Compose官方参考解析库](https://github.com/compose-spec/compose-go)
- **S08** [Axum](https://docs.rs/axum/latest/axum/)
- **S09** [rustix](https://docs.rs/rustix/latest/rustix/)
- **S10** [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/)
- **S11** [oci-spec](https://docs.rs/oci-spec/latest/oci_spec/)
- **S12** [runc终端生命周期](https://raw.githubusercontent.com/opencontainers/runc/main/docs/terminals.md)
- **S13** [BuildKit rootless限制与snapshotter](https://github.com/moby/buildkit/blob/master/docs/rootless.md)
- **S14** [OCI运行时](https://github.com/opencontainers/runc)
- **S15** [Harbor能力声明](https://raw.githubusercontent.com/harbor-framework/harbor/main/src/harbor/environments/capabilities.py)
- **S16** [Docker OverlayFS与页缓存](https://docs.docker.com/engine/storage/drivers/overlayfs-driver/)
- **S17** [OpenSandbox生命周期协议](https://github.com/opensandbox-group/OpenSandbox/blob/main/specs/sandbox-lifecycle.yml)
- **S18** [OpenAI Harness Engineering](https://openai.com/index/harness-engineering/)
- **S19** [OpenAI执行计划](https://developers.openai.com/cookbook/articles/codex_exec_plans)
- **S20** [Harbor TaskConfig](https://raw.githubusercontent.com/harbor-framework/harbor/main/src/harbor/models/task/config.py)
- **S21** [Anthropic长程开发代理harness](https://www.anthropic.com/engineering/effective-harnesses-for-long-running-agents)

## 必须补做

T01与T33在可联网的授权开发机实际下载/检出、校验clean tree、生成文件hash、安装依赖并验证对应接口。`0.20.0`仅为观察到的Harbor主分支元数据，不能据此假定同名wheel和主分支一致。Python可运行seed通过的测试记录与真实上游验收分开保存。

没有引用未给出原文的内部规范。未取得完整基准数据/镜像/有效解法前，不发布任何全量通过率或性能结果。
