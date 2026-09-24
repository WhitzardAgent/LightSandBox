# 原生v1离线样例

镜像引用使用example.invalid及演示digest，故意不可拉取。样例用于字段与CLI计划测试，不代表运行资产。

```bash
python3 tools/lsbx.py --json env plan --file examples/native/environment.json
python3 tools/lsbx.py --json exec --generation 1 --dry-run sb_demo -- printf "%s" "a b"
```

真实运行前必须通过获准来源准备镜像并创建服务。完整网络/卷/多服务行为仍由生产环境引擎实现。
