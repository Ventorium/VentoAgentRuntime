# VentoAgentRuntime 实施计划

## 目标

VentoAgentRuntime 是独立的 Rust workspace，向 Bun/Node.js 提供两个 napi-rs 模块：

- `@ventostack/document-runtime`：将企业文档、PDF、网页、图片和音视频转换为带来源与处理决策的 LLM Markdown。
- `@ventostack/vm-runtime`：通过独立控制面管理 Firecracker microVM，并提供 E2B 风格的命令与文件接口。

项目不依赖 anydoc、pdf-inspector、liteparse 等高层转换库。允许使用经锁定和审计的底层格式、网络和系统 crate。

## 交付阶段

1. 建立 Cargo workspace、共享类型、许可证、NOTICE、上游源码映射、依赖策略和 CI。
2. 吸收并重构 anydoc 的文档模型、格式解析器和 Markdown renderer。
3. 吸收并重构 pdf-inspector 的 PDF 字体、布局、表格、文本提取和页面分类能力。
4. 完成路径/URL/bytes 输入，SSRF 与 canonical root 防护，图片 OCR、媒体转写及 Provider 契约。
5. 完成 Sandbox 状态机、幂等创建、资源限制、命令/文件/快照接口和 fake backend 测试。
6. 完成 Firecracker+jailer、reflink 探测、rootfs 克隆、guest agent、控制面和 CLI。
7. 发布两个 napi-rs 包；普通 CI 使用 fake backend，Linux/KVM 自托管 CI 运行真实隔离与快照演示。

## 安全不变量

- 路径输入只能位于显式配置且 canonicalized 的根目录。
- URL 只允许 HTTP(S)，每次重定向重新解析 DNS，并拒绝 loopback、私网、链路本地、ULA 与云元数据地址。
- Provider 缺失时返回 `PROVIDER_REQUIRED`，不合成内容。
- ZIP、下载、Provider JSON、命令输出、文件传输、递归层级和执行时间均必须有上限。
- Sandbox 的状态变更按实例串行化；Secret 不进入持久化快照路径。
- Firecracker 模式只在 Linux + KVM + reflink 文件系统预检成功后启动，不回退为完整复制。

## 验收矩阵

普通 CI 必须通过：`cargo fmt --check`、`cargo clippy --workspace --all-targets -- -D warnings`、`cargo test --workspace`、禁止高层解析依赖检查和 `cargo deny check`。自托管 KVM CI 负责 create、Shell、Python、文件、知识库、联网、隔离和 Snapshot 八类真实演示。

## 首期边界

首期不修改 VentoStack，也不包含 Kubernetes、多节点调度、GPU、非 Linux guest、OverlayBD、ublk、分布式 Volume、Web UI、计费或完整 E2B wire compatibility。

