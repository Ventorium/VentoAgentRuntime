# Debian slim guest image

先为 Linux guest 目标静态编译 `agentd`，再将二进制放入本目录并构建 OCI 镜像。生产流水线应把镜像转换为只读 raw ext4 rootfs，记录内核、rootfs 与 agent 协议版本摘要，并将 `/workspace`、`/knowledge` 作为独立挂载点。

当前运行时通过 virtio-vsock `10000` 端口连接 `agentd`。更新 `vento-agent-protocol`
或 `agentd` 后必须重新生成 rootfs；旧的仅使用 stdin/stdout 的 guest 镜像不能用于真实运行时。
