## Context

当前两端的关闭行为都有问题：

- **客户端**：Ctrl-C 后 `select!` 能返回，但 spawned task（heartbeat / uplink / downlink）的 JoinHandle 被 drop 后变成 detached，没有取消机制。`forward()` 是永不退出的 `loop { recv().await; send().await }`，只能靠 `recv()` 返回 error 才能打破。三个 task 可能在各自的 `await` 上无限期挂起，拖慢进程退出。此外客户端 `endpoint` 在 `establish_connection` 内就 drop 了，退出时无法对它做 `close()`。

- **服务端**：Ctrl-C 后主流程直接返回，所有 `handle_conn` 和全局 `downlink_pump` 都是 detached spawn，不做任何清理就被 OS 强杀。服务端不向客户端发送任何关闭通知，客户端只能靠心跳超时（30s）才感知到。

## Goals / Non-Goals

**Goals:**

- 两端 Ctrl-C 后：打印关闭日志 → 等所有 task 清理（IP 归还、registry 移除、QUIC 连接关闭）→ 进程退出
- 服务端关闭时向客户端发送 `Disconnect { reason: "server-shutdown" }`，客户端收到后立即退出（无需等 30s 心跳超时）
- 关闭有超时保护（5s），保证进程一定能退出
- 所有 `select!` 分支的 cancel-safety 有明确分析

**Non-Goals:**

- 客户端自动重连
- 服务端发完 Disconnect 后等待客户端 ACK
- 客户端三段函数的整体结构重构
- 细粒度关闭进度报告

## Decisions

### D1：取消信号用 CancellationToken，而非 AtomicBool / watch / broadcast

**选择**：`tokio_util::sync::CancellationToken`

**备选方案与排除理由**：

| 方案 | 排除理由 |
|------|---------|
| `AtomicBool` + 轮询 | 没有 future 语义，无法在 `select!` 里直接 await，需要手写 poll 逻辑或额外 channel 配合 |
| `tokio::sync::watch` | 可用但语义不直观（watch 是"值变更"而非"一次性取消"）；每个消费者要持有 receiver 且需处理初始值 |
| `tokio::sync::broadcast` | 每个消费者要克隆 receiver，且 `recv()` 在 Lagged 时报错需处理；API 更重 |
| `tokio::sync::Notify` | 可以用，但一对一语义，多消费者需多次 `notify_waiters` 且不能保证"已取消"状态持久化（后注册的消费者收不到过去的信号） |

CancellationToken 的优势：`cancelled()` 返回的 future cancel-safe（可被 drop 后重新 poll）、clone 廉价、一旦 cancel 后所有后续 `cancelled()` 立即 ready（状态持久化）。它是 tokio 生态管理一组 task 生命周期的标准工具。

**新依赖确认**：项目已依赖 `tokio-util`（`codec` feature），CancellationToken 在 `sync` feature 下，只需加 feature，不引入新 crate。

### D2：forward / downlink_pump 直接改签名加 cancel 参数，而非新建 cancellable 版本

**选择**：修改现有函数签名。

**理由**：`forward` 只有两个调用方（服务端上行泵、客户端上行/下行泵），全部需要取消能力。新建 `forward_cancellable` 会导致两套几乎相同的代码，维护负担更大。签名变更是 **BREAKING** 的（proposal 已标注），但影响面可控。

### D3：服务端用 JoinSet 追踪所有 handle_conn，而非 Vec<JoinHandle>

**选择**：`tokio::task::JoinSet<()>`。

**理由**：`JoinSet` 是 tokio 官方推荐的管理一组 task 的方式，内置 `join_next()` / `abort_all()` / `len()` / `shutdown()` 等能力。手维护 `Vec<JoinHandle>` 需要自己处理已完成 task 的回收和 abort 逻辑，容易出错。

### D4：关闭超时 5 秒，超时后 abort_all

**选择**：`tokio::time::timeout(Duration::from_secs(5), join_all)`，超时后 `abort_all`。

**理由**：正常情况下 cancel 后 task 在毫秒级退出。但如果某个 task 卡在 OS 层面的阻塞 syscall 上（TUN recv 底层），cancel 不一定立即唤醒。5s 是防御性边界——足够慢机器完成清理，又不会让用户等太久。

### D5：服务端关闭时在心跳 task 的 cancel 分支内发送 Disconnect

**选择**：在 ctrl_task 的 `select!` cancel 分支体内发送 `Disconnect { reason: "server-shutdown" }`。

**备选方案与排除理由**：

| 方案 | 排除理由 |
|------|---------|
| 把 writer 从 ctrl_task 移出到 handle_conn，退出前发送 | 需要重构 ctrl_task 的所有权结构，reader/writer 需分离管理，改动量大 |
| 在 handle_conn await ctrl_task 后发送 | ctrl_task 结束时 writer 已被 move 进闭包并 drop，拿不回来 |

在 cancel 分支内发送：writer 还在 ctrl_task 闭包内可用。发送失败（连接已断）时 `let _ =` 忽略——Disconnect 是"尽力而为"的友好通知，不是关键路径。

### D6：客户端 establish_connection 返回 endpoint，延长生命周期到数据面结束

**选择**：`establish_connection` 的返回值从 `(conn, framed, params)` 改为 `(endpoint, conn, framed, params)`，`run_with_credentials` 持有 endpoint 直到 `run_data_plane` 结束。

**理由**：当前 endpoint 在 `establish_connection` 内 drop，退出时无法对它做 `close()`。虽然 `conn.close()` 能关闭连接，但 endpoint 持有的 UDP socket 需要显式 close 才能干净释放。延长生命周期不影响正常逻辑——endpoint 在连接建立后不再被使用，只是活着等退出时 close。

### D7：客户端入口 spawn signal watchdog，尽早注册 SIGINT 捕获

**问题**：rpassword 读取密码时会清除 tty 的 `ISIG` 标志（raw mode）。若用户在**密码输入期间**按 Ctrl+C，终端不产生 SIGINT，`0x03` 被 rpassword 读到后调用 `raise(SIGINT)`。此时客户端尚未在 `run_data_plane` 注册 `ctrl_c()` handler（仍为 SIG_DFL），**进程被信号直接杀死**，rpassword 的 `Drop`（恢复 termios）不会执行，pty 的 `ISIG` 残留为关闭。此后该终端上所有 Ctrl+C 都只产生字节不产生信号，客户端 `ctrl_c()` 永远收不到 → 表现为"Ctrl+C 无反应"，而 `kill -INT <pid>`（绕过终端直接发信号）有效。

**选择**：在 `client::run` 入口调用 `spawn_signal_watchdog()`（`tokio::spawn` 一个 task 注册 SIGINT handler，收到后打印 `received Ctrl+C, initiating graceful shutdown` 并 `shutdown.cancel()`），将返回的 `CancellationToken` 贯穿 `run_with_credentials` → `run_data_plane`。密码读取改为 `tokio::task::spawn_blocking(rpassword::prompt_password)`，使 main task 让出 runtime、watchdog 尽快完成 handler 注册。

**备选方案与排除理由**：

| 方案 | 排除理由 |
|------|---------|
| 保持现状，仅在 `run_data_plane` 注册 handler | 密码输入阶段仍无 handler，Ctrl+C 会 SIG_DFL 杀死进程并残留 `-isig` |
| 在 `main.rs` 注册 | 逻辑散落在 main，且客户端与服务端共用入口，职责不清；放 `client::run` 更内聚 |
| 客户端主动 `tcsetattr` 恢复 ISIG | 侵入用户终端状态，且无法保证与 rpassword 的 raw mode 时序正确叠加 |

**效果**：
- 密码输入期间 Ctrl+C → `raise(SIGINT)` 被 watchdog 捕获（不杀死进程）→ rpassword 返回 `Interrupted` → 其 `Drop` 恢复 termios → 优雅退出，终端不残留 `-isig`
- 运行中 Ctrl+C → watchdog 打日志 + cancel → `run_data_plane` 的 `shutdown.cancelled()` 分支触发关闭
- `run_data_plane` 保留兜底 `ctrl_c()` 分支（handler 注册失败时仍可响应）

**cancel-safety**：watchdog task 内 `signal().recv()`（`Signal::recv`）cancel-safe（tokio 官方确认）。watchdog 只消费信号并 cancel token，不共享 `&mut` 状态。

## Cancel-safety 分析

> 规则要求：涉并发的部分必须说明 cancel-safety。

### forward 改造后的 cancel-safety

```
loop {
    let pkt = tokio::select! {      // ← select! 内
        biased;
        () = cancel.cancelled() => return Ok(()),
        pkt = source.recv() => pkt?,
    };
    sink.send(pkt).await;            // ← select! 外
}
```

- `cancel.cancelled()` — **cancel-safe**：CancellationToken 内部用 `Notify` 实现，被 drop 后重新 poll 不会丢失取消事件。
- `source.recv()`（QuinnDatagram）— **cancel-safe**：quinn 的 `Connection::read_datagram()` 官方文档确认 cancel-safe。
- `source.recv()`（TunSource）— **cancel-safe**：每次分配新 buf，cancel 后丢弃 buf，无跨 await 的残留状态。
- `sink.send(pkt).await` — 不在 select! 内，不受 select cancel 影响。若外层 future 被 abort，pkt 丢失可接受（IP 包本就可丢）。

### heartbeat_loop 加 cancel 分支后的 cancel-safety

现有代码已有 cancel-safety 注释（reader 与 writer 各自独立 Framed，HeartbeatTracker 仅被无 await 的 timeout 分支借用）。新增 cancel 分支为 biased 最高优先级，不引入新的 `&mut` 借用冲突：

```
tokio::select! {
    biased;
    _ = cancel.cancelled() => { /* 发 Disconnect(break) */ }  // 新增
    _ = timeout_tick.tick() => { tracker.is_dead(...) }       // &mut tracker, 无 await
    _ = send_tick.tick() => { writer.send(hb).await }         // &mut writer
    msg = reader.next() => { /* match msg */ }                // &mut reader
}
```

- `cancel.cancelled()` — cancel-safe（同上）。
- cancel 分支体内的 `writer.send(Disconnect).await` — writer 在此分支独占使用，与其他分支无并发 `&mut` 借用。

### 服务端 run() 关闭流程的 cancel-safety

```
tokio::select! {
    _ = ctrl_c() => { shutdown.cancel(); }
    _ = accept_loop => { shutdown.cancel(); }
}
// 以下不在 select! 内：
endpoint.close(...)
timeout(5s, join conn_set)  // join_next() 是 cancel-safe
```

- `ctrl_c()` future — cancel-safe（tokio 官方确认）。
- `accept_loop` 内部的 `endpoint.accept()` — cancel-safe（quinn 确认）。
- `conn_set.join_next()` — cancel-safe（JoinSet 官方确认）。

### 客户端关闭流程（入口 watchdog + run_data_plane）的 cancel-safety

```
// 入口（run 内，密码读取前）：
spawn_signal_watchdog()  // spawn 一个 task：
                         //   signal(SIGINT).recv().await → 打日志 → shutdown.cancel()
                         //   signal().recv() — cancel-safe（tokio 官方确认）

// run_data_plane：
tokio::select! {
    biased;
    () = shutdown.cancelled() => {}          // watchdog / 任一 task 已 cancel
    _ = tokio::signal::ctrl_c() => { shutdown.cancel(); }  // 兜底
}
// 以下不在 select! 内：
conn.close(...)
timeout(5s, join_set)
endpoint.close()
```

- 入口 watchdog 的 `signal().recv()` — cancel-safe。
- `run_data_plane` 的 `ctrl_c()` — cancel-safe。
- `shutdown.cancelled()` — cancel-safe（CancellationToken 官方确认）。
- watchdog 先注册 SIGINT handler（密码读取用 `spawn_blocking` 让出 runtime 保证尽快注册），密码输入期间 Ctrl+C 的 `raise(SIGINT)` 被捕获而非 SIG_DFL 杀死进程，rpassword 的 `Drop` 得以恢复 termios，终端不残留 `-isig`。
- 任一 task 先结束触发 `shutdown.cancel()`，其余 task 收到信号退出，join 时它们已完成。

## Risks / Trade-offs

- **[TUN recv 不可取消]** TUN 设备的底层 syscall（read）在 cancel 后不一定立即返回（取决于平台和驱动） → 5s 超时 + abort_all 兜底，保证进程一定能退出。正常情况下 quinn 的 read_datagram cancel 后立即返回，只有 TUN 侧可能慢。

- **[Disconnect 发送不可靠]** cancel 分支内发 Disconnect 可能失败（连接已断 / writer 出错） → 这是"尽力而为"通知，失败时客户端靠心跳超时（30s）兜底。不阻塞服务端退出。

- **[forward 签名 BREAKING]** 所有 forward 调用方都要改 → 影响面可控（服务端上行泵 + 客户端上下行泵），在 tasks.md 中列为前置步骤。

- **[JoinSet 在 accept loop 内 spawn 的所有权问题]** accept_loop 闭包需要持有 conn_set → 用 `&mut JoinSet` 引用传入闭包，或用 `Arc<Mutex<JoinSet>>`。前者更简单（accept_loop 不跨线程，`&mut` 足够），实现时选 `&mut`。

- **[已残留 -isig 的终端无法被客户端自救]** 若某个终端此前已被 SIG_DFL 杀死 rpassword 进程而残留 `-isig`，watchdog 也无法让当前 Ctrl+C 生成信号（信号在终端层就没产生）。→ 用户需在该终端执行 `stty isig` 一次性恢复；D7 保证**今后**不会再有新的残留。
