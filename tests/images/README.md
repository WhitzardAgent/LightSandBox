# tests/images — T06 contract fixtures

## entrypoint_matrix.json

T06-C02 组合矩阵:image ENTRYPOINT/CMD × native override(Inherit/Clear/Replace)→
期望 entrypoint/command/argv。语义锚点:`docs/v5/03_DOMAIN.md`(launch_mode=image
保持原镜像入口;override null/缺失=继承,[]=清空)与 OCI argv 规则
(argv = Entrypoint ++ Cmd)。Docker CLI 的 `--entrypoint` 重置 CMD 细则由
适配器编译,**不**出现在此矩阵中(03_DOMAIN 同上)。消费方:
`crates/ls-host/src/images/process_config.rs` 的
`c02_entrypoint_cmd_combination_matrix_matches_fixture`。

## OCI layout 运行时夹具

T06 的 digest/导入/并发/平台用例在测试时用
`crates/ls-host/src/images/mod.rs::testkit` **现场构建**确定性 OCI layout
(单层、opaque payload;T06 不运行/不解包层内容,unpack 归后续任务),
不落二进制 blob 进 git。腐败用例(l1 flip one byte)由测试按需生成。

## C05 受认证 registry — live 拉取状态

本宿主目前**没有任何 pinned registry transfer tool**:
`/opt/toolchains/lightsandbox/bin/` = runc/run-seccomp/scmp_sys_resolver/gperf/umoci。
umoci 只做本地 layout,不从 registry 拉取。按 T01 source-lock 规则,
不允许临时抓一个未锁 skopeo 二进制上执行路径。因此:

- 合同面(pinned digest 校验、tag→digest 解析、authfile 0600、argv/日志/错误
  无秘密、env_clear)由 `pull.rs` 单测覆盖,FileTransport 走完整 fetch 路径;
- **真实受认证 registry 拉取 = BLOCKED**(需要授权 registry + pinned
  transfer tool,且需要 worker/网络授权),不作为本任务的隐式通过项;
  SkopeoTransport 在无二进制时如实返回 `IMAGE_TRANSPORT_UNAVAILABLE`。
