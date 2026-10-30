# feat(link): 移动设备链路与远控服务（ADB/usbmuxd/WebRTC + screen/input）

状态：已发布为 [#956](https://github.com/arcships/rutis/issues/956)。优先级 P2。依赖契约 issue 与 `link/tcp`。

## 问题

手机/设备是 rutis 的重要场景：设备上的插件提供相机、传感器、通知、屏幕、输入；宿主在笔记本/服务器/云上。需要两件事，不能混在一起：

1. **设备链路**：怎么从宿主连到设备，或从设备连回宿主。ADB 的 `forward`/`reverse`、iOS 的 `usbmuxd`/`iproxy`、WebRTC data channel 都能给通用通道。
2. **远控能力**：屏幕、输入、音频、剪贴板是**应用服务**，不是 link layer。RDP/VNC/SPICE/RustDesk 是远控应用协议，不要把它们强行做成 link。

现在没有这两层的清晰边界，容易把 RDP/VNC 当 link，或者把 ADB 只当调试工具。

## 范围

1. `link/adb`（Android）：
   - `adb forward tcp:<port> tcp:<port>` / `adb reverse` 给本地 TCP；
   - `link/tcp` bridge 接 session；
   - 准入由 ADB key/设备授权负责；不把 ADB 身份传给 session；
   - 文档写清 USB 调试、设备授权、生产限制。
2. `link/usbmuxd`（iOS）：
   - `iproxy <local> <remote>` 给本地 TCP；
   - `link/tcp` bridge 接 session；
   - 准入由 iOS 信任/配对负责；
   - 文档写清 Apple 限制、开发/生产边界。
3. `link/webrtc`（可选，先原型）：
   - WebRTC data channel 作为通用消息通道；
   - signaling 由已有的 SSH/Tailscale/Cloudflare/gRPC 提供；
   - STUN/TURN、DTLS 身份、浏览器/移动端限制写清。
4. 远控服务（应用层，不是 link）：
   - 定义 `screen` / `input` / `audio` / `clipboard` 服务的形状（可以先最小 `screen` + `input`）；
   - 远控 plugin 可以用 RDP/VNC/SPICE/RustDesk/scrcpy/WebRTC 实现；
   - 其他插件通过服务使用远控，不直接依赖协议。
5. 文档：`docs/guide/link-plugins.md` 增加“移动设备链路”和“远控是应用服务”两节；明确 RDP/VNC 大多数不是 link。

## 非目标

- 不把 RDP/VNC/SPICE 做成通用 link layer。
- 不重新实现 RDP/VNC 编解码；集成现成实现。
- 不先做完整远控产品；先做服务契约和最小实现。
- 不把 ADB/WebRTC 身份映射成 session identity。

## 验收

- [ ] Android：`adb forward` + `link/tcp` 跑通 session；设备上的插件以 `<id>:<plugin>` 出现。
- [ ] iOS：`iproxy` + `link/tcp` 跑通（有 macOS/Xcode 环境时）；文档写清限制。
- [ ] WebRTC data channel 至少一个 loopback 原型跑通；signaling/TURN 写清。
- [ ] `screen` / `input` 服务最小实现：跨设备调用、权限/错误清楚；不依赖 link 层。
- [ ] 文档明确“RDP/VNC 是应用协议，不是 link”；RDP DVC 只作为例外提一句。
- [ ] `cargo test` / 语言测试 / 文档链接检查通过。

## 关联

- [链接层 link plugin 契约](01-link-plugin-contract.md)
- [`link/tcp` 与反向隧道](06-link-tcp-and-reverse-tunnels.md)
- [`link/iroh` 与 P2P](09-link-iroh-and-p2p.md)
- [Android 应用托管](../../design-android-runtime-2026-10-12.md)
