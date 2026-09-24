# 进程、恢复、事务与取消的困难实现

## 1. runc命令返回不等于guest生命周期被管理

runc提供低层OCI运行；上层仍须负责stdin/stdout/PTY、退出状态观察、孤儿进程、重连和删除。其终端文档明确说明调用方与分离模式的责任。[S01/S02]

首发supervisor按sandbox划分。在授权环境实现：父子关系/subreaper策略；CLOEXEC继承表；console-socket的SCM_RIGHTS接收；pidfd可用则采用，否则PID+starttime+boot_id联合核对；二进制helper不得在多线程Tokio进程里直接setns/fork后运行不安全代码。

不能把任意JSON回执当作进程退出证据。退出记录由真正观察到wait status或受信supervisor事件的路径创建，先落盘再宣告完成。exec启动时先记录launch intent和唯一exec id，再调用runc。启动成功与收据写入之间的宕机窗口仍可能不确定；以实际进程身份对账。

## 2. 故障域合同

| 故障 | 允许恢复什么 | 不允许声称什么 |
|---|---|---|
| 客户端断线 | 重连同exec、按cursor取输出 | 自动重跑同命令 |
| API重启 | 重建请求路由、读取manager状态 | generation全部更新 |
| manager重启 | 重连survivor supervisors并核对资源 | DB显示RUNNING就一定还活着 |
| supervisor死亡 | 隔离该sandbox、查收据/cgroup、告知unknown | 凭PID猜完整输出与exit 0 |
| VM重启 | 基于持久化制品创建新实例，清理残留 | 恢复原RAM/PTY/TCP |

删除时先阻止新exec/lease请求、撤销入口访问；保留必要的控制通路用于清理。终止进程之后，端点/网络、挂载/卷、CAS引用按依赖逆序处理。cgroup残留、挂载busy、CNI DEL失败均产生CLEANUP_REQUIRED，预算不能提前释放。

## 3. 意图与外部副作用

每个创建步骤保存资源归属标签/私有路径。intent提交→外部动作→实际收据→状态提交。数据库和Linux没有统一ACID事务，采用可恢复的补偿流程。失败时同时报告原始错误与清理错误，不能让后者覆盖前者。

unknown仅阻止重复副作用，不阻止晚到真实收据的确认。reconcile必须校验owner、sandbox代次、exec身份、证据摘要及签发来源，并产生新审计记录。

幂等键保留期覆盖对象寿命与最大重试窗口；被GC的幂等记录必须有tombstone/明确过期返回，不能把旧key当新执行。首次实施可以延长保留而不做复杂GC。

## 4. 取消与终端

普通process取消可能只终止目标进程；tree取消要有受验证的执行子cgroup或等效作用域；sandbox取消会影响同沙盒其它任务和sidecar控制，必须显式授权。不能因为子cgroup难做，就默认使用kill(-pgid)并承诺终止全部子孙。

PTY Ctrl-C应按终端前台进程组行为传播，窗口resize作用于同一master FD。持久shell、PTY、非交互exec分别定义，不能用每条bash -lc模拟持久会话。sandbox停止不代表所有输出都已刷到客户端，退出收据与流完成偏移分别确认。

## 5. 日志与背压

分开控制面日志、guest输出、构建输出、验证制品。输出事件队列/磁盘spool/订阅者均有预算；慢消费者不阻塞全节点。达到保留限额返回STREAM_GAP或OUTPUT_LIMIT并给可取范围，不能静默截断又宣告完整。

禁止无限存储每个event的hash到单进程内存；参考ExecAccumulator只适用于有界测试。生产用分段chunk摘要与索引，SQLite保存块引用，不逐字节写事务。shared broker优化不得先于故障恢复证明。

## 6. 必须先做的故障注入

对每一个intent/effect/receipt边界中断API/manager；重复发同key；先记录exit再断socket；迟到收据；supervisor单独被杀；exec双重fork；PTY断线与resize；volume仍被子进程持有；CNI DEL超时；磁盘满；SQLite busy；并发删除与exec。

这些测试需要专用获准VM。不能在用户工作站或共享CI上运行随机kill、挂载、网络规则修改。
