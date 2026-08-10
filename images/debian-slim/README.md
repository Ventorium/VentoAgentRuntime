# Debian slim guest image

先为 Linux guest 目标静态编译 `agentd`，再将二进制放入本目录并构建 OCI 镜像。生产流水线应把镜像转换为只读 raw ext4 rootfs，记录内核、rootfs 与 agent 协议版本摘要，并将 `/workspace`、`/knowledge` 作为独立挂载点。

