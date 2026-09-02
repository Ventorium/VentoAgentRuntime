# VM 105 Agent Runtime 部署与运维

本文记录 2026-09-02 在 Proxmox VE 虚拟机 105 上验证通过的 Vento Agent Runtime 部署。

## 已验证环境

- PVE：`root@192.168.10.10`
- VM：ID 105，主机名 `vento-runtime-kvm`，地址 `192.168.10.210`
- VM 日常 SSH 用户：`vento`
- Firecracker / jailer：1.16.1
- Rust：1.94.1
- 虚拟化：嵌套 KVM，VM 内存在可读写的 `/dev/kvm`
- cgroup：cgroup v2
- 数据盘：`/vento`，XFS，支持 reflink

从本机经 PVE 登录 VM：

```bash
ssh root@192.168.10.10
ssh vento@192.168.10.210
```

## 部署布局

```text
/vento/VentoAgentRuntime-current/          当前源码和构建产物
/vento/firecracker-assets/vmlinux.bin      guest kernel
/vento/firecracker-assets/vento-rootfs.ext4
/vento/vento-firecracker-next.json         Firecracker 配置
/vento/runtime-data/                       jailer、沙箱和快照数据
/vento/runtime-data/control-plane.json     原子持久化的控制面索引，0600
/etc/vento-agent-runtime.env               Bearer token，root:root 0600
/etc/systemd/system/vento-agent-runtime.service
```

Firecracker 配置使用 UID/GID 1000 运行 jailer，核心路径如下：

```json
{
  "firecrackerBinary": "/usr/local/bin/firecracker",
  "jailerBinary": "/usr/local/bin/jailer",
  "jailerUid": 1000,
  "jailerGid": 1000,
  "kernelImage": "/vento/firecracker-assets/vmlinux.bin",
  "baseRootfs": "/vento/firecracker-assets/vento-rootfs.ext4",
  "templateRootfs": {},
  "dataDir": "/vento/runtime-data"
}
```

实际配置以 `/vento/vento-firecracker-next.json` 为准。

## 构建与 guest agent

服务端在 VM 上构建，不在 macOS 本地生成 `target`：

```bash
cd /vento/VentoAgentRuntime-current
export PATH=/home/vento/.cargo/bin:$PATH
cargo build --release --bin vento-runtime-server
```

guest rootfs 中的 `/agentd` 必须是适用于 guest 架构的静态 Linux 可执行文件。更新前先对 XFS 上的镜像做 reflink 备份，停止 runtime，并在卸载状态下用 `debugfs` 或 loop mount 替换文件；完成后运行 `e2fsck -f`。当前保留的回滚点：

```text
/vento/firecracker-assets/vento-rootfs.ext4.pre-e2fsck
/vento/firecracker-assets/vento-rootfs.ext4.pre-agentd-update
/vento/firecracker-assets/vento-rootfs.ext4.pre-entropy-fix
/vento/firecracker-assets/vento-rootfs.ext4.pre-control-plane-persistence
```

不要在服务运行或仍有 Firecracker 进程持有根盘时修改基础镜像。

## systemd 服务

当前服务以 root 运行，因为 jailer 初始化、cgroup 和设备准备需要宿主权限；Firecracker 本身由 jailer 降权到配置的 UID/GID。服务仅监听 loopback，不直接暴露到局域网。

```ini
[Unit]
Description=Vento Agent Runtime (Firecracker)
After=local-fs.target
ConditionPathExists=/dev/kvm

[Service]
Type=simple
User=root
Group=root
WorkingDirectory=/vento/VentoAgentRuntime-current
EnvironmentFile=/etc/vento-agent-runtime.env
Environment=PATH=/home/vento/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
ExecStart=/vento/VentoAgentRuntime-current/target/release/vento-runtime-server --listen 127.0.0.1:8088 --firecracker-config /vento/vento-firecracker-next.json
Restart=on-failure
RestartSec=2s
KillMode=control-group
LimitNOFILE=65536

[Install]
WantedBy=multi-user.target
```

常用操作：

```bash
sudo systemctl status vento-agent-runtime
sudo systemctl restart vento-agent-runtime
sudo journalctl -u vento-agent-runtime -f
curl -i http://127.0.0.1:8088/health
```

健康接口应返回 `204`。API token 不写入本文；root 可从 `/etc/vento-agent-runtime.env` 读取。需要轮换时：

```bash
sudo systemctl stop vento-agent-runtime
sudo sh -c 'umask 077; openssl rand -hex 32 | sed "s/^/VENTO_RUNTIME_TOKEN=/" > /etc/vento-agent-runtime.env'
sudo chown root:root /etc/vento-agent-runtime.env
sudo chmod 0600 /etc/vento-agent-runtime.env
sudo systemctl start vento-agent-runtime
```

如需从本地调用，优先使用 SSH 端口转发：

```bash
ssh -L 8088:127.0.0.1:8088 vento@192.168.10.210
```

## 验收流程

宿主能力验收：

```bash
cd /vento/VentoAgentRuntime-current
sudo env \
  HOME=/root \
  PATH=/home/vento/.cargo/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin \
  VENTO_FIRECRACKER_CONFIG=/vento/vento-firecracker-next.json \
  bash tests/sandbox/run-host-acceptance.sh
```

它验证 `/dev/kvm`、cgroup v2、XFS/Btrfs reflink 和 Firecracker 配置。上线前的真实门禁还必须覆盖：

1. 创建 Firecracker sandbox。
2. 通过 guest `agentd` 执行命令并校验 stdout/exit code。
3. 写入并读回 guest 文件。
4. pause 后 resume，并再次读回原文件。
5. 创建持久快照。
6. 销毁 sandbox，确认服务仍为 `active`。

本次上述全部步骤均通过。相关分层测试说明见 [sandbox acceptance](../tests/sandbox/README.md)。`vento-agentd` 的 host contract 在普通 `vento` 用户下有一项宿主 `RLIMIT_NPROC=31722` 与测试请求的 32768 不匹配；guest 内 agentd 以 root 运行，真实 microVM 门禁已通过，因此这不是当前部署阻塞项。

## 服务重启恢复

控制面将 sandbox、snapshot 和幂等键索引原子写入 `/vento/runtime-data/control-plane.json`。systemd 发出 `SIGTERM` 时，服务先要求 guest 执行 `sync(2)`，再为无 secrets 的运行态 VM 创建 pause checkpoint，然后才退出。新进程启动时：

- 运行态 VM 从 shutdown checkpoint 自动恢复为 `RUNNING`。
- 暂停态 VM 保持 `PAUSED`，后续 `resume` 复用其 checkpoint。
- 停止态和失败态对象恢复为可查询、可销毁的控制面对象。
- snapshot 元数据恢复后仍可查询，也可以用 `snapshotId` 创建新 VM。
- sandbox rootfs、内存、控制面 ID、幂等键和文件内容均跨服务重启保留。

`secrets` 永远不会写入控制面状态文件。包含 secrets 的 sandbox 重启后会标记为 `FAILED`，并要求使用者重新创建和重新注入 secrets，避免在磁盘上泄露凭据或静默以缺失凭据的环境运行。

已在 VM 105 上通过真实门禁：写入文件、创建 snapshot、`systemctl restart`、查询原 sandbox/snapshot、在原 sandbox 执行命令和读回文件、从重启前 snapshot 创建新 sandbox 并读回同一文件、最后销毁两个 sandbox。

## 本次修复和故障排查

为跑通真实门禁，源码中修复了以下问题：

- 将运行时的下划线 sandbox ID 映射为 jailer 接受的连字符 ID。
- Firecracker Unix HTTP 客户端读取完整响应头，不再依赖服务端主动断开 keep-alive 连接。
- jailer 重启前只清理瞬态的 socket、PID、`dev`、`run` 等文件，保留根盘、kernel 和快照。
- guest agent 的 command ID 改用时间戳和进程内原子序列，避免缺少熵时阻塞在随机数生成。
- 从 pause 快照恢复时，reflink 生成的新根盘重新设置为 jailer UID/GID，避免 `/snapshot/load` 报 `/rootfs.ext4: Permission denied`。
- 创建持久快照后使用 `PATCH /vm {state: Resumed}` 恢复当前 VM；不能对已经启动的同一 VM 调用 `/snapshot/load`。

快速诊断顺序：

```bash
sudo systemctl is-active vento-agent-runtime
curl -i http://127.0.0.1:8088/health
sudo journalctl -u vento-agent-runtime -n 200 --no-pager
sudo pgrep -a firecracker
sudo find /vento/runtime-data/jailer -maxdepth 5 -name rootfs.ext4 -printf '%M %u:%g %p\n'
```

若部署新二进制，先构建 release，再执行 `systemctl restart`；只运行 `cargo test` 不会更新 systemd 使用的 `target/release/vento-runtime-server`。
