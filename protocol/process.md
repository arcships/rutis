# Linux 冻结进程与回收证据

`process::FrozenProcess` 从 `SnapshotGroup` 启动真实 Rust/Node runtime，并在私有
连接关闭后独立回收受管后代。它是 M3 的 OS 组件；完整 epoch supervisor、更新
回滚和消费者清理屏障仍在开发，不能据此重开旧 instance 或宣布组恢复完成。

## 启动与私有连接

Host 调用 `FrozenProcess::launch(reaper_path, group, options)`。`reaper_path` 必须
指向 Host 信任的 SDK `rutis-protocol-reaper` 二进制；执行文件、Node catalog 和
runner 路径来自冻结组，`LaunchOptions` 的参数、环境及断连宽限由 Host 决定，
不是插件配置或另一条业务控制入口。描述符在创建 helper 前序列化并检查 16 MiB
限制。helper 只接收一次描述符，不支持换代或重新绑定代码。

返回值包含 `ProcessHandle`、业务 Unix stream 和 stdout/stderr 诊断流。业务
runtime 继承 fd 3；helper 启动与回收证据使用另一个私有 fd 4，fd 4 在执行
runtime 时关闭。两个源 fd 先复制到保留位置，再设置目标 fd，避免覆盖彼此。
stdout/stderr 不承载任何回收证明。Node 的默认 catalog 自动附加，Rust 的真实
静态 factories 与 hello/RuntimeReady 仍由既有 lifecycle API 准入。

`handle.attach(&peer)` 把同一 SDK 私有 peer 接入退出通知。`terminate()` 在调用点
同步关闭 peer，使 HostObjects、RuntimeReady 与已经捕获 provider 的原生消费者
先关闭准入，再请求 OS 终止。主进程退出也先关闭 peer。peer 自行断连时，独立
任务等待默认 200 ms 宽限；显式 terminate 或最后一个 handle 被丢弃直接终止。
这段等待不 await native disposer，故卡住的原生清理不能阻止 OS 回收。

## 实际后代回收

每个组由独立的单线程 helper 设置 `PR_SET_CHILD_SUBREAPER`。Host 本身不设置
全局 subreaper，也不调用 `waitpid(-1)` 争抢其他模块的子进程。孤儿后代归最近
仍存活的祖先 subreaper，helper 可以等待它们的退出；这是 Linux 的
[subreaper 约定](https://man7.org/linux/man-pages/man2/PR_SET_CHILD_SUBREAPER.2const.html)。
实际用例另验证子、孙进程各自 `setsid` 后仍被这个 helper 回收。

helper 是唯一 waiter。收到终止或主 runtime 退出后，循环读取自己的直接子进程，
向未回收的子进程发送 SIGKILL，再等待、接收成为孤儿的后代并重复。读取到的
子进程在唯一 waiter 回收前仍占用其 PID，不按历史 PID 列表或进程组猜测归属。
只有实际 `waitpid` 返回 ECHILD 且取得原始 runtime 的退出状态，才发出 Reaped。
Host 还必须等待 helper 正常退出，才构造私有字段的 `Reaped` receipt。关闭连接、
主进程退出或 `/proc` 中暂时无成员，都不能独立构造这个 receipt。

这套组件依赖 Linux、可用的 procfs 和正确的 Host helper；它管理普通 fork/exec
后代，不宣称 OS 沙箱、资源配额或完整隔离。helper 通道丢失时继续终止及回收；
helper 自身失败、kill/wait/procfs 错误或 Host 未取得证明时保守地报告未确认，
不会从错误中推断剩余后代已退出。exec 仅对 ETXTBSY 最多重试 8 次，累计退避
255 ms；其他错误或重试耗尽仍是启动失败，不重试业务或自动创建新 epoch。

## 等待、租约与未确认结果

`handle.reaping()` 产生可克隆的 receipt waiter，`wait()` 与 `handle.reaped()`
观察同一个缓存结果。waiter 不拥有进程存活租约，丢弃 waiter 不取消回收；最后
一个 handle 丢弃后，任务仍独立推进。启动 future 丢弃时关闭描述符写半边，
helper 要么确认尚未创建 runtime，要么回收已经创建的进程。正常启动失败只有
在可信 helper 确认未创建 runtime 且退出成功后，才释放启动租约。

OS worker 持有自己的 `SnapshotGroup`，Reaped 后才释放。错误、任务中断或缺少
证明时，该租约保留在本 Host 生命周期的 quarantine 中；
`quarantined_snapshots()` 只提供诊断路径，没有强制成功或清除隔离的 API。
`ProcessStatus` 的 Starting/Running/Reaping/Reaped/FailedToLaunch/Quarantined
是这个组件的观测状态，不是完整 supervisor 的恢复状态。

原生消费者另外拥有自己的快照租约，直到实际 disposer 完成才释放。因此 OS
Reaped 可以先到，而消费者仍继续保留冻结目录。当前调用方必须显式管理这些
消费者租约。[HostGraph 清理观察](dependency-cleanup.md) 已记录实际消费者代的
清理结果；尚未实现 supervisor 自动持有全部旧成员/失效消费者租约、合并两道屏障
与签发新代。丢失远端 native stop ACK 仍使 HostProxy stop 失败，
Reaped 不伪造该 ACK，不开放新的 instance 或 epoch。

`handle.attach_epoch(&host, identity, &peer)` 为 Host 将这个实际私有连接绑定到固定
runtime/epoch，并返回 `EpochReaping`。调用方必须使用本 launch 的 stream 或其
Host transport wrapper；一份 launch 只能绑定一次，不能重绑另一个 epoch 或 Host。
OS receipt 与绑定共享独立的 launch 身份，Host 按这个身份核对，而不是按可能复用
的 PID 猜测。绑定与结算只由 Host API 建立，业务帧不能选择代或提交回收证明。

独立任务在实际 OS receipt 后确认相同 epoch 已断连，才结算 broker 中以该死亡
epoch 为 owner 的调用；`ReapedEpoch` 缓存完整身份、OS receipt 与结算数量。丢弃
`EpochReaping` waiter 不停止结算，重复 join 返回同一结果。死亡进程只是 caller 时，
其他存活 owner 的执行 pin 保留到实际 finished ACK，新代仍可因这份在途执行被
拒绝。普通超时、连接关闭、未完成查询的 waiter 丢弃和 native stop 错误都不构成
这份 OS 证明。原生消费者清理与全组恢复许可仍是另外的屏障。

## 验收证据与剩余范围

Linux `native_runner_ipc` 的原有正常停止用例通过同一 helper 启动冻结 Rust/Node
镜像，继续验证 hello、双向 DI、真实服务、延迟 Accept/activate ACK、正常 native
cleanup 和退出码。原有六项中另含 child-entry 与本地缺少 export 的清理测试；
新增四项实际用例：

- Node provider 生成脱离会话的子、孙进程；强制退出关闭真实 HostGraph 与原生
  消费者，旧 Ctx 拒绝 effect。OS receipt 和四个 PID 的实际消失先于受控慢 disposer；
  另让真实查询在 handler 内永久等待，核对实际 execution pin。丢弃 epoch waiter
  后终止，OS 回收独立结算该 pin，重复 receipt 仍报告一次结算；查询失败，不重放。
  原生消费者清理 receipt 保持未完成，快照继续存在；放行实际 disposer 后才确认
  清理并删除目录，旧 instance 仍不能重挂。
- 丢弃一个 receipt waiter 不影响 Running；最后一个进程 handle 丢弃后仍完成实际
  回收，两份 receipt 得到同一结果。实际 runtime 的 `/proc/.../fd` 另确认保留
  业务 socket，却未继承 helper 证据 socket 的任何别名。进程回收等待有独立的
  5 秒测试期限。
- 以已知不会 fork 的 `/usr/bin/true` 替代 helper，缺少回收证明触发 quarantine 并
  保留目录。只有该 fixture 清理自己的文件；这不是实际 kill 权限失败的证据。
- 在大描述符启动尚未返回 receipt 时丢弃 future；真实 helper 确认未启动或回收，
  不遗留启动租约，快照在独立 5 秒测试期限内释放。

大型镜像 fixture 最多两个并行，以控制同时复制文件的峰值；各用例内部的多个
真实 runtime、双向调用和停止交错保持并发。child-entry 是子进程入口，不能另算
一个独立运行时验收结果。

M3 仍须完成：全组旧成员/消费者清理与 OS 回收的恢复屏障、死亡 owner 结算的完整
跨语言故障交错、精确新 epoch、配置/代码/runner 更新及回滚、StopUnconfirmed 的
显式等待/继续、RecoveryBlocked/Quarantined 管理与持久诊断，以及实际 OS 回收
失败注入。这些证据覆盖 T20/T23 的部分平台行为，不宣称完整 T20、T23 或 M3
通过，M0–M5 / T01–T24 的目标保持不变。
