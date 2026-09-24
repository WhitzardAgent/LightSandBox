# 原始来源与证据边界

读取日期：2026-09-24。以下为公开原始文档/源码入口；`main/master/latest`是可变入口，均属于观察资料，不是可复现源码锁。生产T01/T33仍须锁完整commit、wheel及文件摘要。本文对设计取舍的建议为本项目分析，不声称上游作者认可。

## S01

OCI Runtime lifecycle / runc

https://raw.githubusercontent.com/opencontainers/runtime-spec/main/runtime.md

用途：OCI create/start/delete与配置生效边界；不是完整container manager。

## S02

runc terminal management

https://raw.githubusercontent.com/opencontainers/runc/main/docs/terminals.md

用途：FD、终端、监督和分离执行责任。

## S03

OCI image-to-runtime conversion

https://raw.githubusercontent.com/opencontainers/image-spec/main/conversion.md

用途：镜像进程配置转换；不等于所有Docker扩展。

## S04

BuildKit upstream README

https://raw.githubusercontent.com/moby/buildkit/master/README.md

用途：独立daemon、OCI(runc)worker与OCI导出。

## S05

Docker OverlayFS driver

https://docs.docker.com/engine/storage/drivers/overlayfs-driver/

用途：Docker共享层、页缓存和存储驱动行为。

## S06

OpenSandbox architecture

https://raw.githubusercontent.com/opensandbox-group/OpenSandbox/main/docs/architecture/index.md

用途：协议门面、Docker/Kubernetes、execd与endpoint分工。

## S07

OpenSandbox lifecycle and execd contracts

https://github.com/opensandbox-group/OpenSandbox/blob/main/specs/sandbox-lifecycle.yml

用途：实际协议仍须源码和SDK锁；本文不是它的复制。

## S08

rustix documentation

https://docs.rs/rustix/latest/rustix/

用途：Unix/Linux安全封装；版本随发布变化。

## S09

HTTPX transports

https://www.python-httpx.org/advanced/transports/

用途：UDS及MockTransport；mock不证明真实运行。

## S10

Harbor task structure

https://www.harborframework.com/docs/tasks

用途：task.toml/environment，独立验证、产物与sidecar。

## S11

Harbor BaseEnvironment

https://raw.githubusercontent.com/harbor-framework/harbor/main/src/harbor/environments/base.py

用途：实际环境接口，应在固定安装中测试。

## S12

Harbor environment factory

https://raw.githubusercontent.com/harbor-framework/harbor/main/src/harbor/environments/factory.py

用途：第三方import path入口。

## S13

Harbor capability contracts

https://raw.githubusercontent.com/harbor-framework/harbor/main/src/harbor/environments/capabilities.py

用途：Compose要求per-service操作；网络能力细分。

## S14

CNI specification

https://www.cni.dev/docs/spec/

用途：网络插件接口；完整DNS/策略需要上层实现。

## S15

compose-go reference parser

https://github.com/compose-spec/compose-go

用途：规范化复用参考库；不是直接执行环境。

## S16

OpenAI harness engineering

https://openai.com/index/harness-engineering/

用途：简短AGENTS、仓库知识、上下文渐进加载与机器检查。
