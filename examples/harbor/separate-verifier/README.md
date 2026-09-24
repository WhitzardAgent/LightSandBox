# 独立verifier与产物传递合成canary

本例在task.toml顶层声明`artifacts = ["/workspace/result.txt"]`。按本轮观察到的Harbor任务文档，独立verifier会在原绝对路径重新物化该制品，因此同一个测试脚本检查此文件的实际内容。agent与verifier使用不同Dockerfile，tests脚本预置在verifier镜像中。

本包只做只读解析/语法检查，没有运行Harbor或镜像。本例是待验证的端到端canary，不是已通过的正式任务。镜像与源码必须在T33/T37锁定后真实运行；也要运行缺失/错误制品的负控，不能只看进程退出码。
