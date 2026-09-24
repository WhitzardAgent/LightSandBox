# LightSandbox v5：架构收敛与接口原型

**2026-09-24｜独立包｜设计、参考代码与开发工作包；尚无完整运行时。**

目标保持：普通Linux虚机、无KVM、无需更换内核；以runc/OCI实现沙盒，独立BuildKit处理构建；OpenSandbox兼容接口与Harbor环境后端；最终以完整固定CyberGym、SWE-bench、Terminal-Bench任务集验收。必要宿主能力/管理权限仍必须具备。

## 本次实质修订

24项审计问题收敛到一套native v1领域模型、CLI/SDK/HTTP合同；剥离核心verifier语义；明确启动模式、故障域、supervisor成本及幂等/输出/取消边界。CLI和SDK附有可执行参考代码与测试。完整Harbor加载、真实沙盒与全量兼容没有被宣称已实现。

全部文件包含在本ZIP内；无需下载旧版或单独文档。旧版规范迁移为导航，原文保存在history用于审计，不再形成双重实施要求。

## 建议阅读

[产品目标](docs/v5/00_PRODUCT.md) · [审计与修订](docs/v5/01_AUDIT.md) · [架构](docs/v5/02_ARCHITECTURE.md) · [CLI](docs/v5/04_CLI.md) · [SDK](docs/v5/05_SDK.md) · [Harbor](docs/v5/07_HARBOR.md) · [技术栈](docs/v5/10_TECH_STACK.md)

[START_HERE](START_HERE.md)提供开发入口；[INDEX](INDEX.md)列全包导航；[ROADMAP](ROADMAP.md)说明45个任务的执行顺序。具体产品测试规格和源锁未完成状态保留在specs与state中。

## 当前可执行

```bash
python3 tools/packctl.py validate
python3 dev/check_native.py
python3 dev/check_seeds.py
python3 -m unittest discover -s tests/pack -v
python3 tools/lsbx.py --json version
python3 tools/lsbx.py --json env plan --file examples/native/environment.json
python3 tools/lsbx.py --json exec --generation 1 --dry-run sb_demo -- printf '%s' 'a b'
```

当前native客户端的HTTP请求需要真实兼容服务；本包没有启动服务端。`env plan`和`--dry-run`只做本地计划。样例image使用不可拉取的example.invalid地址，不能当运行资产。测试依赖见dev/requirements-reference.txt。

## 验证边界

最终测试数量、命令与限制见[evidence/v5/VALIDATION.md](evidence/v5/VALIDATION.md)。规划校验和reference测试不证明runc隔离、上游SDK互操作或benchmark成功。Rust参考源码未编译的状态保留；所有产品任务仍需真实证据。`release-check`预期阻断未完成发行。

原貌校验用`python3 tools/packctl.py verify-manifest`；开发后正常修改会使旧manifest失配，不能通过重写原始证据来掩盖变更。
