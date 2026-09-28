# 可立即运行的规划工具

仅依赖Python 3.10+标准库。源码锁辅助需要本地Git。工具不联网、不安装软件、不创建沙盒、不修改内核/网络策略。

```bash
python3 tools/packctl.py validate
python3 tools/packctl.py status
python3 tools/packctl.py next
python3 tools/packctl.py context T14
python3 tools/doctor_readonly.py
python3 -m unittest discover -s tests/pack -v
```

`validate`检查任务ID、依赖DAG、需求/测试映射、文档链接、状态与证据结构；`next`只列依赖已核验的planned任务；`context`输出当前任务需要的读取范围，不一次塞入全部文档。

`release-check`在本包初始状态应非零退出，因为产品、来源锁、宿主和完整基准证据均未完成。它只作结构阻断；正式发行仍由T32可信CI与独立审查完成。

`doctor_readonly`只读取有限宿主信息与可执行文件是否存在，不运行runc/probe，输出始终为NOT_PROBED。带 `--output` 时拒绝覆盖已有文件。可以把该输出给T03代理，但不能把它当完整能力证明。

## 本地源码锁

```bash
python3 tools/lock_local_sources.py \
  --repo opensandbox=/opt/src/OpenSandbox \
  --repo runc=/opt/src/runc \
  --output /tmp/source-observations.json
```

目录必须为实际干净Git checkout，origin匹配登记来源。工具只读Git中HEAD对应字节并计算哈希，不fetch、不安装、不执行上游项目。上述仅两仓库输入会得到PARTIAL；只有所有登记源都提供才可产生完整source lock。SDK分发、镜像、二进制、数据集仍需单独冻结。示例路径需替换为机器实际路径。

## 归档校验

`python3 tools/packctl.py verify-manifest`核对初始交付文件集合与每个SHA-256，忽略Git内部目录和Python缓存。开始正常开发后失配是预期现象，不应为掩盖开发修改而更改原始归档。日常工作用validate。

## 文档工具

以下三者为 docs CI 服务，仅依赖标准库，不联网：

- `tools/check_md_links.py`：校验 `docs/**/*.md` 与根 `README.md` 的相对 Markdown 链接指向存在的文件；`--snippets` 模式校验 bash 代码块仅使用仓库真实入口（`tools/*.py`、`dev/check*`、`python3 tools/*`、cargo test/build、环境命令），不写不存在的二进制名。
- `tools/gen_error_docs.py`：从 `crates/ls-api` 错误码源码、参考客户端源码与 `contracts/v1` 拒绝码生成 `docs/reference/errors.md`；`--check` 重新生成并比对漂移。
- `tools/gen_capability_matrix.py`：从 `contracts/v1` 需求维度与解析规则、ls-core 后端 AUTO 顺序、`specs/host-profiles.json` 层级生成 `docs/reference/capability-matrix.md`；`--check` 同样做漂移比对。两者改动源文件后必须重新生成，由 `dev/check docs` 强制。

## 可信性边界

这些工具只能发现结构错误与不一致，不能抵抗拥有文件写权限的人同时篡改工具、状态和证据。关键门禁需要受保护CI/分支、独立review和制品存储控制。本包没有通用任务执行器，也没有自动sudo/远程部署动作。
