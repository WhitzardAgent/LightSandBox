# T10 前台执行 / 幂等身份 / 真实退出结果 — EXPECTATIONS

任务卡:`plans/tasks/T10.md`。范围:`crates/ls-host/src/exec/`、`crates/ls-api/src/commands/`、`tests/runtime/exec/`(本文件)。
规范钉点:docs/v5/03_DOMAIN.md §命令与退出、§幂等与超时;06_HTTP.md §输出与重放、§统一错误;08_FAILURES.md §退出证据、§取消。

## 实现形状(与任务卡范围对齐)

- **见证执行(字节/退出双保真)**:`RuncWitnessExecPort` 在 `runc exec` 的 argv 前注入 T08 的
  `/.lsbx/init`(witness),命令为
  ``runc exec <ctr> /.lsbx/init --sandbox-id <id> --receipt /.lsbx/exec/<exec_id>.receipt.json -- <argv...>``。
  POSIX `128+n` 与 `exit(128+n)` 在 runc CLI 语义上不可区分;退出互斥联合(Exited/Signaled)由
  guest 内真 waitpid 收据裁断,宿主侧 `native::parse_receipt` strict 解析 + sandbox_id 归属校验 +
  **CLI 回传码交叉校验**(Exited→code&0xff;Signaled→128+(sig&0x7f);SpawnFailed→127),不一致即
  InternalFailure(协议污染,绝不静默裁断)。
- **凭收据不凭文本**:收据缺失/不可解析/张冠李戴 → `WitnessLost{cli_exit}` → 域状态 `unknown`
  (与 T08 超时律同源),永不凭 CLI 码猜退出。
- **字节保真**:stdout/stderr 以 `Vec<u8>` 双管道有界捕获(`MAX_CAPTURE_BYTES=128KiB`+截断标记+drain
  防死锁),全程不经过 T08 backend 的 `from_utf8_lossy` String 表面(该表面是字节有损的,见任务评审)。
- **端口镜像纪律**:witness port 自建 `Command`,与 `RuncRuntime::exec_command` 在共享段
  (`--root exec --cwd --env <id> <argv>`、`env_clear`+`PATH/HOME/XDG_RUNTIME_DIR`、pre_exec
  setgid/setuid)**逐参数一致**;漂移由单测 `build_command_mirrors_runc_exec_command`
  (`Command::get_args/get_envs` 全等断言)守死。`exec_command` 不支持 `--user`,故 v1 不发明新语义。
- **用户语义(rootfs 内解析)**:`resolve_user` 数字 `u[:g]` 直取;名称查沙箱 rootfs 自有
  `/etc/passwd`/`/etc/group`(绝不问宿主);passwd 缺失仅容忍字面名 `root`→(0,0),其余→NotFound。
  **PORTABLE_ROOTLESS 单 ID 映射下,非 0:0 请求建前 PolicyUnsupported 拒绝、不造 operation**——
  这就是 T10-C01 非 root UID 的诚实臂(它不是"执行成功",是"拒绝证据齐全")。
- **幂等身份(冻结域复用)**:`IdempotencyScope`(principal+v1+POST+/v1/executions+sandbox_id+key)
  + `request_digest(canonical_body(spec))`(ls-core T04 已测 canonical_json/sha256;env 序无关)。
  Fresh 在**任何副作用前**落占位(starting),同一锁内 decide;并发重发看到 `starting` 与同一
  execution id,副作用恰好一次(单测实证);同键异体 → `ErrorCode::Conflict`(409),禁忌换键重试。
- **状态机折叠**:accepted→starting→running→exited(+start-failed/unknown)全部由 ls-core
  `Execution` 域对象驱动(finish_exited/finish_signaled/fail_start/finish_unknown/reconcile);
  超时为**独立终止维**:先 `mark_termination(Timeout)` 后 `finish_unknown`(终态后标记是 CONFLICT,
  顺序不可反)。
- **wire 层(ls-api)**:body 严格解析直接复用冻结源 `ls_core::contracts::ExecSpec::parse`;
  `ExecOperationWire.exit` 为 `deny_unknown_fields` 的互斥联合;`termination` 词汇封闭;
  错误信封 `code=ErrorCode::wire()`、`http_status=ErrorCode::http_status()`(冻结 12 码映射);
  base64 为模块内手写 RFC4648 §4(带 padding,strict 解码),跨 chunk UTF-8 解码态在 SDK 侧(06_HTTP)。
- **流事件**:v1 前台执行交付终态缓冲(本模块);token 化流事件(execution_id/generation/sequence/
  kind/stream)是 facade 未来面,不在 T10 范围,06_HTTP 的"生产设计未实现"注记不变。

## 证据(真实宿主,2026-09-26)

宿主:PORTABLE_ROOTLESS 全链(65534:65534),cgroupfs 只读(T09 测量),runc 钉装
`/opt/toolchains/lightsandbox/bin/runc`,witness 为 musl static-pie
`target/x86_64-unknown-linux-musl/debug/lsguest-init`(ADR 0001 A4;替代过渡期的 gnu
crt-static 路径)。

### 真实层(LSB_T10_REAL=1;`cargo test -p ls-host --test exec -- --test-threads=1`)

**4/4 通过,3.78s**(串行)。裁判语义逐卡:

| 卡 | 实测 |
|---|---|
| T10-C01 | 空格参数+env+cwd 字节级回环:`M=m a r k|A=value with spaces|C=/tmp` 与期望完全相等(字节对比);`--sandbox-id` 收据含 `init_waitpid`;`user:"1000:1000"` → PolicyUnsupported,message 含 "maps exactly one uid"(拒绝且不造 operation;重放同错);`user:"0:0"` 正常执行(`ROOT0-OK`)。 |
| T10-C02 | stdout=`[ff fe fd 41 00 42]`、stderr=`[80 81 7a]` **raw bytes 完全相等**(非 UTF8,不经任何 lossy 转换);`exit 7` 经 wait 路径;>128KiB 输出(≈202KB)→ `stdout.len()==131072==MAX_CAPTURE_BYTES` 且 `stdout_truncated==true`;wall < 30s。base64 线编码锁在 ls-api 单测(同字节 `operation_wire` 往返相等)。 |
| T10-C03 | 副作用命令 `printf X >> /.lsbx/mrk`:同 key 重发 → 同一 execution id、`replayed=true`,宿主侧读 rootfs 文件 = `X`(恰好一次);异 key 重发 → 新 id,文件 = `XX`;同 key 异体 → Conflict(409 语义),文件保持 `XX`(零副作用)。 |
| T10-C04 | `/bin/definitely-not-there` → start-failed(SpawnFailed{Precheck},detail 含 ENOENT);rootfs 内 0644 文件 → start-failed(Precheck, 含 execute bit);相对名 `definitely-no-such-binary-xyzzy` → start-failed,**witness 收据 SPAWN_FAILED**(原文归档含 `SPAWN_FAILED`);`kill -SEGV $$` → **Exited + Signaled{11}**,收据由真 waitpid 出,runc CLI 139 仅作交叉校验证据;`timeout_ms=400` 死循环 → **Unknown + TerminationReason::Timeout**,wall∈(350,10000)ms(强制生效非时钟漂移),SIGKILL 进程组;重放 → `starting` 占位返回,不重跑。 |

### 单测

- ls-host `exec::`:**15/15**(validate/canonical/env 序无关/shell 物化/resolve_user rootfs 语义/
  非 root 拒绝/Exited/SpawnFailed/Signaled/TimedOut/WitnessLost 折叠/重放同 operation 单次副作用/
  Conflict/并发占位/precheck 真实 fs 元数据/runc 镜像 drift guard)。
- ls-api `commands::`:**9/9**(冻结夹具 26/26 cases 全 agree(ACCEPT 含 normalized 对比)、路由
  边界、非 UTF8 wire 字节往返、互斥联合 deny_unknown_fields、错误码/状态映射、termination 封闭词汇、
  base64 RFC4648 KAT/任意字节往返/strict 拒绝)。

### 门禁(权威解释器 `/usr/bin/python3.12`)

- `cargo test --workspace`:**276 passed / 0 failed**(ls-core 120、ls-host lib 107、ls-api 16、
  ls-guest 12、integration lifecycle 11+resources 6+exec 4)。
- `tools/packctl.py validate`:errors: []。
- `dev/check_native.py`:**76 OK**;`dev/check_seeds.py`:**109 OK**;`tests/pack`:**34 OK**。
- `./dev/check fast`:**suite=fast exit=0, tests_total=377, failures=0**(workspace 276 + harness/python-dev/pack/contract-locks;内部固定 `/usr/bin/python3.12` 权威面)。

**Known gap(既有,非 T10 引入,如实留痕)**:裸 `python3` 指向 3.10.12,`check_native/check_seeds`
在其下成批 ERROR(import/语法级),权威面是 `/usr/bin/python3.12`。与 T09 的 349/349 同一环境约定。

### 如实披露

- `runc exec` 的丢失/权限/SIGSEGV/timeout 行为即本 REAL 层首次运行本身(镇测 = 上述实测证据);
  未使用任何文本猜测做分类。
- 执行注册表为**进程内存 v1**(崩溃即失;耐久 journal 归 T43 supervisor 面,在任务卡外如实披露)。
- 超时杀的是 runc CLI 的进程组(SIGKILL -pgid);容器内孙进程经 PID ns 一并终结(best-effort,
  孤儿化风险与 witness 化复核归 T43 设计更新)。
- T10 不改 `runtime/{backend,runc,manager,native/mod}`(任务卡范围外事);manager 不持有 exec v2
  入口,facade 接线刻意留给后续任务。

## 门禁命令(复跑)

```bash
cargo test --workspace
LSB_T10_REAL=1 cargo test -p ls-host --test exec -- --test-threads=1 --nocapture
/usr/bin/python3.12 dev/check_native.py
/usr/bin/python3.12 dev/check_seeds.py
/usr/bin/python3.12 -m unittest discover -s tests/pack
```
