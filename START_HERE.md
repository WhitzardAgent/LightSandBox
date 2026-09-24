# 从这里开始

1. 读AGENTS.md与[产品边界](docs/v5/00_PRODUCT.md)，再读[修订审计](docs/v5/01_AUDIT.md)。
2. 运行下列命令，确认包和参考代码实际状态。
3. 领取唯一当前任务，按其输入/输出/文件边界实施，不一次生成完整平台。

```bash
python3 tools/packctl.py validate
python3 tools/packctl.py next
python3 tools/packctl.py context T00
python3 dev/check_native.py
python3 dev/check_seeds.py
python3 -m unittest discover -s tests/pack -v
```

客户端环境要求Python>=3.11和HTTPX；schema测试需要jsonschema。当前环境已装的精确版本见dev/requirements-reference.txt，安装依赖只在开发者批准的环境进行。

当前CLI只支持严格离线计划和部分真实HTTP请求。服务端缺失就报错，dry-run返回executed=false。完整一键Harbor创建与原生runc服务是产品任务。

规范优先级：项目目标及验收合同→docs/v5→contracts/v1（具体字段）→任务catalog→局部实现。发现正文/schema冲突，应先修订合同和交叉测试，不能选择容易的版本偷偷继续。历史源位于history，不具有规范优先级。

[完整导航](INDEX.md)｜[路线图](ROADMAP.md)｜[首次启动指令](launch/BOOTSTRAP.md)｜[Harbor专项](launch/HARBOR.md)。
