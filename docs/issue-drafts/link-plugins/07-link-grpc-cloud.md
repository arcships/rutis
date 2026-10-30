# feat(link): `link/grpc` 云控制面 / relay / 分布式云

状态：已发布为 [#955](https://github.com/arcships/rutis/issues/955)。优先级 P1（云业务）/ P2（其他）。依赖契约 issue。

## 问题

gRPC 不是 Tailscale/Cloudflare 那种“到达”方案，而是“到一个可达云端点之后”的应用协议。它适合：

- rutis 宿主主动连云控制面；
- 宿主 ↔ 宿主 via 云 relay；
- 宿主 → 云运行时（插件跑在云上）；
- K8s/service mesh 内部；
- 分布式云的多区域 relay / 故障切换。

现在没有 `link/grpc`，用户要么自己写 gRPC 适配，要么把 HTTP/2 bidi stream 硬塞进 WebSocket。需要把云链接的契约、限制和产品边界写清楚。

## 范围

1. 新增 `link/grpc` 参考实现（先 Go 或 Rust）：
   - 一条 gRPC bidi stream 对应一个 session；
   - 消息用 `bytes`（或 `google.protobuf.BytesValue`）承载；
   - 每条连接/流一个 `sessions.open(far)`；
   - mTLS / OAuth / JWT / API key 做链接层准入；不把身份传给 session；
   - 远端在 `Hello` 注册 id。
2. 云端形态（文档 + 测试服务，不必先做产品）：
   - 控制面：宿主注册、路由、配额、观测；
   - relay：两条流配对，多租户；
   - 云运行时：gRPC 流到 rutis 运行时；
   - 多区域：最近区域、会话粘性、故障切换、数据驻留。
3. 技术约束写清：
   - HTTP/2 中间层/负载均衡的超时、缓冲、最大流时长；
   - 默认 gRPC 消息上限（一般 4 MiB）与 rutis 16 MiB 的差异；
   - 长连接与 serverless scale-to-zero；
   - gRPC-Web 不支持 bidi；浏览器用 WebSocket/WebTransport；
   - keepalive、重连、会话粘性。
4. 文档：`docs/guide/link-plugins.md` 增加“云链接”一节；明确 gRPC 是云链接，不是设备链接。

## 非目标

- 不把 gRPC 做成 NAT 穿透/设备互联方案。
- 不把 gRPC 身份映射成 session identity。
- 不先建完整多租户云产品；先做协议适配和测试服务。
- 不把 gRPC-Web 当浏览器 bidi 方案。

## 验收

- [ ] 本地 gRPC 测试服务：宿主 dial、bidi stream、`sessions.open`、远端插件 `<id>:<plugin>` 跑通。
- [ ] mTLS 或 OAuth 准入失败时连接被拒；session 不感知。
- [ ] 一条流断开 → session 卸载；重连在 link plugin；云 relay 能重新配对。
- [ ] 16 MiB 消息在调大 gRPC 限制后跑通，或明确分片策略。
- [ ] 文档写清云控制面/relay/云运行时/多区域四种场景和限制。
- [ ] 测试不依赖真实云账号；`cargo test` / 语言测试 / 文档链接检查通过。

## 关联

- [链接层 link plugin 契约](01-link-plugin-contract.md)
- [`link/tcp` 与反向隧道](06-link-tcp-and-reverse-tunnels.md)
- [连接节点](../../guide/nodes.md)
