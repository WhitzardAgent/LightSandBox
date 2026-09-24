# 合成样例

`minimal`：单环境中的文件Oracle与共享验证。

`multiservice`：main、HTTP sidecar、健康依赖、named volume，测试实际HTTP请求与共享卷内容。

`separate-verifier`：不同验证镜像，通过顶层artifacts传递agent产物并校验内容。

本包仅完成配置/目录检查，没有构建或执行Harbor任务；它们不是三个正式基准的替代品。镜像tag尚未固定digest，运行门禁须先锁上游和镜像。
