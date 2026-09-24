# Rootless Capability Probe — 宿主只读探测报告

日期：2026-09-24｜方式：纯只读探测（未提权、未改 `/etc`/sysctl/systemd/LSM、未安装包、未下载二进制）｜临时文件已清理。
对齐对象：`plans/tasks/T03.md` 的被动前置观察（不等同 T03 完成）。

## 0. 身份观察（与委托前提的差异，如实记录）

委托前提写明"当前用户没有 root、没有 sudo"。实测当前 shell 是 **uid=0(root)**，但处在**嵌套容器**内，能力面等价于受限 rootless 用户：

- `CapEff = CapBnd = 0x00000000a80425fb`（Docker 默认能力集：**无 CAP_SYS_ADMIN、无 CAP_NET_ADMIN**)
- cgroupfs 只读、`/proc` 存在 Docker 式 masked 路径、无 `/dev/kvm`、无 `/dev/fuse`、`dmesg` EPERM
- hostname=`cyber-copy-copy-c-copy--23b7716a310a-vgtowbpznu`，根文件系统为 overlayfs
- AppArmor 标签：`unconfined`（宿主侧 AppArmor enabled=Y)

## 1. 关键原始输出

### Identity / OS

```text
uid=0(root) gid=0(root) groups=0(root)
Linux ... 5.15.0-119-generic #129-Ubuntu SMP x86_64
Ubuntu 22.04.4 LTS (jammy) | GLIBC 2.35 | nproc=128 | Mem 1.5Ti(1.4Ti avail) | / 7.0T(4.9T avail)
```

### User namespaces

```text
unshare -Ur true                                  -> exit=0
unshare -Ur sh -c 'id; cat uid_map; cat gid_map'  -> uid=0(root); "0 0 1"; "0 0 1"; exit=0
/proc/sys/kernel/unprivileged_userns_clone         -> 1
/proc/sys/user/max_user_namespaces                 -> 6190011
/proc/sys/kernel/unprivileged_userns_apparmor_policy -> cat: Operation not permitted(本层被屏蔽)
新userns内 CapEff = 000001ffffffffff(完整 capability)
```

### subordinate IDs / uidmap helpers

```text
/etc/subuid, /etc/subgid -> 文件为空,无任何用户条目
newuidmap -> MISSING    newgidmap -> MISSING
```

### OCI runtime

```text
runc/crun/podman/buildah/skopeo/umoci/buildctl/buildkitd -> 全部 MISSING
(未执行 runc spec,无二进制)
```

### cgroup

```text
cgroup on /sys/fs/cgroup type cgroup2 (ro,nosuid,nodev,noexec,relatime)
/sys/fs/cgroup/cgroup.controllers -> cpuset cpu io memory hugetlb pids rdma misc
/proc/self/cgroup -> 0::/
cgroup.subtree_control -> (空)
mkdir /sys/fs/cgroup/lsbx-probe -> Read-only file system(BLOCKED)
maxdepth>=1 可写 cgroup 文件: 无
user.slice: 无;systemctl --user: 无 bus 连接失败
```

### filesystem

```text
/proc/filesystems: nodev overlay / fuseblk / nodev fuse / nodev fusectl
/dev/fuse -> 不存在        fuse-overlayfs -> MISSING
$HOME: overlayfs(df: overlay on /,7.0T)
```

### networking helpers

```text
rootlesskit / slirp4netns / pasta / passt / lxc-user-nic -> 全部 MISSING
ip 命令 MISSING
unshare -Urn -> 成功创建 netns,仅有 lo(net:[4026547088])
```

### security

```text
/proc/self/status: Seccomp 0 | NoNewPrivs 0
/sys/module/apparmor/parameters/enabled: Y(securityfs profiles 在本层不可读)
getenforce(selinux): MISSING
/dev/kvm: 不存在
```

### 深度真实探测(userns+mountns,propagation private,均未触碰宿主全局挂载)

| 操作 | 结果 |
|---|---|
| unshare -Urm / -Urpf / -Urn | OK |
| mount tmpfs | OK |
| mount ramfs | OK |
| mount --bind(内容回读) | OK |
| overlay,upperdir 在磁盘(本机磁盘=overlayfs) | **FAIL**(wrong fs type… overlay-on-overlay upper 被拒) |
| overlay,upperdir 在 tmpfs | OK(RAM 回写层 COW 可用) |
| 新挂载 proc 到全新目录 | **FAIL: permission denied** |
| 新挂载 sysfs | **FAIL: permission denied** |
| unshare --mount-proc(重挂继承的 /proc) | FAIL(继承自外层容器的 masked /proc 挂载导致) |

## 2. 十个问题的回答

1. **Unprivileged user namespace 可用?** 是。`unshare -Ur` 成功、旋钮为 1、上限 619 万、新 userns 内 capability 完整。**但**仅单 identity 映射（uid_map `0 0 1`)，无法表达多 uid/gid。
2. **有 subordinate UID/GID range?** 没有。`/etc/subuid`、`/etc/subgid` 均无任何条目。
3. **newuidmap/newgidmap 可用?** 不可用，两者均缺失。
4. **runc rootless 原则上可用?** 部分。userns/mountns 内 tmpfs、ramfs、bind、overlay(tmpfs upper)、netns 均可用；但 runc 缺失（可用户态静态安装）、**procfs 挂载 EPERM**(runc 默认 `/proc` 在本机无法建立）、sysfs EPERM、磁盘持久 upper 的 overlay EINVAL、cgroup 只读。结论：本机当前形态**跑不起 runc 默认 bundle**;proc 与磁盘 upper 的失败强烈疑似"嵌套容器层"所致，普通 VM 目标环境不必然存在。
5. **cgroup v2 哪些 controller 真正委托给当前用户？** **零**。ro 挂载、subtree_control 空、无可写文件、mkdir→EROFS、无 user.slice、无 user bus。资源硬限额必须声明 HOST_UNSUPPORTED 直至管理员委托。
6. **rootless filesystem 可用路径？** tmpfs/ramfs(RAM，可 size= 会计）、bind、overlay(tmpfs upper 的 RAM COW)、整树复制的目录 rootfs。不可用：磁盘 upper 的 overlay、fuse-overlayfs（无 /dev/fuse)、新 proc/sysfs。
7. **rootless networking helper?** 当前为零（全部缺失）；netns 可建但仅 lo。用户目录静态安装 slirp4netns/pasta 可提供 NAT 出口（**本轮仅报告可能性，未下载**)。
8. **Tier 判定：Tier B(Minimal Rootless）偏下限**；就"原生 rootless runc 默认配置"而言，本机当前形态呈现 **Tier C 特征**。理由：命名空间原语可用（非硬阻断），但多 uid 映射缺失、cgroup 零委托、procfs 挂载失败三项叠加，使默认 OCI 生命周期不可原样成立；其中一部分可用户态补齐，一部分只能 admin/换宿主形态解决。
9. **对三大基准的预期限制:**
   - **SWE-bench**：镜像族多且部分需多 uid、构建期需网络出口、多数镜像要 `/proc` — 本机当前形态预计大面积进 blocked 台账；用户态补齐 runc+slirp4netns 后可跑单 uid 子集；密度运行无 cgroup 硬限，只能以观测控噪。
   - **Terminal-Bench**:PTY/交互可用；需要 systemd、Docker-in-task、自定义网桥的任务失败；网络类任务依赖 slirp/pasta；可运行小子集。
   - **CyberGym**：常见服务监听+定制网络，部分环境预期特权/KVM — 无 CAP_NET_ADMIN、无 KVM、proc 受限，预计较多任务保留 blocked；纯用户态服务类可尝试。
10. **只能由宿主管理员预先配置的问题：** subuid/subgid 分配；newuidmap/newgidmap 安全 setuid 安装；cgroup v2 子树委托；`/dev/fuse` 设备暴露；宿主侧解除 `/proc` masked 路径；任何 sysctl/AppArmor/LSM 调整；内核模块。用户态可装（本轮未装）：静态 runc/crun、slirp4netns/pasta、rootlesskit、fuse-overlayfs（无 /dev/fuse 前无效）、静态 buildkitd。

## 3. 对 T03 验收点的映射（观察，非任务完成）

| 验收点 | 观察 |
|---|---|
| T03-C01 KVM-free | /dev/kvm 不存在；探测全程未尝试 KVM;userns/mountns 原语可用 |
| T03-C02 cgroup 不可写 | 复现：ro+EROFS ⇒ 合同要求拒绝声明 hard limit 支持 |
| T03-C03 LSM/策略阻断 | 复现：ns 内 proc/sysfs 挂载 EPERM，已记录，未改任何策略 |
| T03-C04 异常清理 | /tmp/lsbx-probe 已删除；未触碰其他资源 |

**状态边界：** 本报告把 host 从 NOT_PROBED 推进到"已做只读探测"，不构成 runtime-verified；T03 正式执行仍需授权 worker 与产品 runner。
