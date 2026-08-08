## MODIFIED Requirements

### Requirement: 通用单向转发泵

系统 SHALL 提供 `forward<S: PacketSource + Unpin, K: PacketSink + Unpin>(&mut source, &mut sink, cancel: CancellationToken) -> io::Result<()>`，循环执行 `source.recv().await` 后将所得包原样 `sink.send().await`，逐包搬运不做加工。退出条件有二：(1) `source.recv()` 返回 `Err` 时退出并返回该错误；(2) `cancel` 被取消时干净退出并返回 `Ok(())`。cancel 与 recv 的竞争通过 `tokio::select!` 以 `biased` 优先 cancel 分支解决，确保取消信号不被遗漏。cancel 触发时正在 recv 中尚未完成的包（若有）SHALL 被丢弃——等价于 IP 包丢失，上层协议自行处理，不会产生半包写入。`sink.send()` SHALL NOT 在 `select!` 内编排（避免半包写入），SHALL 在 select! 确定 pkt 后单独 await。

#### Scenario: source 的包逐个原样到达 sink

- **WHEN** mock source 预设包 P1、P2 后关闭，以一个未取消的 CancellationToken 调用 `forward(&mut source, &mut sink, &cancel)`
- **THEN** sink 收到 P1、P2 两个包（字节完全相同），随后 forward 因 source 错误返回 `Err`

#### Scenario: source 首次即出错则 sink 无包且返回错误

- **WHEN** mock source 首次 `recv` 即返回 `Err`，以未取消的 CancellationToken 调用 `forward`
- **THEN** sink 未收到任何包，forward 返回该 `Err`

#### Scenario: cancel 后 forward 干净返回 Ok

- **WHEN** mock source 持续产生包但不关闭（`recv().await` 挂起等待），mock sink 正常接收，在 `forward` 运行期间触发 `cancel.cancel()`
- **THEN** `forward` 在 cancel 后迅速返回 `Ok(())`；cancel 之前 sink 已收到的包保持完整；cancel 之后无新的包被 send

#### Scenario: cancel 与 recv 同时就绪时 cancel 优先

- **WHEN** mock source 有一个待读包 P，且 `cancel` 在同一轮 poll 中被取消
- **THEN** `biased` select! 优先处理 cancel，`forward` 返回 `Ok(())`，P 不被处理（等价于丢包）

### Requirement: 服务端下行分发泵

系统 SHALL 提供 `DownlinkDispatcher` trait（方法 `dispatch(&self, pkt: Bytes) -> impl Future<Output = ()> + Send`）与 `downlink_pump<S: PacketSource + Unpin, D: DownlinkDispatcher>(&mut tun, &dispatcher, cancel: CancellationToken) -> io::Result<()>`。下行泵循环执行 `tun.recv().await` 后将包交 `dispatcher.dispatch().await`，逐包处理不加工。退出条件有二：(1) `tun.recv()` 返回 `Err` 时退出并返回该错误；(2) `cancel` 被取消时干净退出并返回 `Ok(())`。`dispatch` 返回 `()`（best-effort），单个包的路由 miss 或发送失败 SHALL NOT 终止下行泵。

#### Scenario: TUN 收到的包原样到达 dispatcher

- **WHEN** mock TUN 预设包 P 后关闭，以未取消的 CancellationToken 调用 `downlink_pump(&mut tun, &mock_dispatcher, &cancel)`
- **THEN** mock_dispatcher 收到与 P 字节完全相同的包，随后 downlink_pump 因 TUN 错误返回

#### Scenario: dispatcher 不影响泵在 TUN 出错前持续运行

- **WHEN** mock TUN 预设包 P1、P2 后关闭，mock_dispatcher 对每个包均返回 `()`，以未取消的 CancellationToken 调用 `downlink_pump`
- **THEN** dispatcher 收到 P1、P2 两个包，downlink_pump 因 TUN 错误返回（dispatcher 的 `()` 返回不导致提前退出）

#### Scenario: cancel 后下行泵干净返回 Ok

- **WHEN** mock TUN 持续有包但 `recv().await` 挂起，在 `downlink_pump` 运行期间触发 `cancel.cancel()`
- **THEN** `downlink_pump` 返回 `Ok(())`，不再处理后续包
