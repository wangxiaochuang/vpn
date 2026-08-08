# 发布前测试清单（Q3 探索性测试）

这份清单用于版本发布前的人工验证。Q3 象限的测试无法自动化覆盖，
依赖真实环境、真实网络条件下的探索性测试。

使用方式：发布 PR 上复制本清单，逐项勾选，附测试环境信息。

## 测试环境

- 服务端 OS：
- 客户端 OS：
- 网络环境（家宽 / 4G / 公司网 / 受限网络）：
- VPN 版本：

## 跨平台 TUN 真机

- [ ] 以 root 运行 `vpn server --config server.toml`，确认 TUN 设备被创建，其 IPv4 地址等于配置 subnet 的网关（池首地址 `.1`），掩码等于 subnet 前缀，MTU 等于配置值（默认 1280）
- [ ] Linux：TUN 设备创建 + IP forwarding + NAT 配置后，客户端能上网
- [ ] macOS：utun 设备创建成功（注意 utun 命名限制）
- [ ] 服务端 TUN subnet 内网关地址可达（ping gateway）
- [ ] 客户端能 ping 服务端网关地址

## 服务端启动流程（Q3）

1. [ ] 生成或准备自签证书：`cargo run --example tlsgen`（产出 `cert.pem` / `key.pem`）
2. [ ] 编写 `server.toml`（参考 `doc/arch-v1.md` §9），含 `[server]` 段与至少一个 `[[users]]`
3. [ ] `cargo build --release --bin vpn`
4. [ ] 以 root 运行：`sudo ./target/release/vpn server --config server.toml`
5. [ ] 确认 tracing 输出含 `listening on <addr>`，进程进入 accept loop 阻塞
6. [ ] 按 Ctrl+C，确认进程干净退出（endpoint close，无 panic）

### OS IP forwarding / NAT 配置

#### Linux

```bash
# 开启 IP forwarding
echo 1 | sudo tee /proc/sys/net/ipv4/ip_forward

# 假设出口网卡为 eth0，TUN subnet 为 10.0.0.0/24
sudo iptables -t nat -A POSTROUTING -s 10.0.0.0/24 -o eth0 -j MASQUERADE
sudo iptables -A FORWARD -i tun0 -o eth0 -j ACCEPT
sudo iptables -A FORWARD -i eth0 -o tun0 -m state --state RELATED,ESTABLISHED -j ACCEPT
```

#### macOS

```bash
# 开启 IP forwarding
sudo sysctl -w net.inet.ip.forwarding=1

# NAT 规则（假设出口网卡为 en0）
echo "nat on en0 from 10.0.0.0/24 to any -> (en0)" | sudo pfctl -ef -
```

### 端到端 ping 测试

1. [ ] 服务端启动后，客户端连接并完成认证
2. [ ] 客户端 TUN 拿到虚拟 IP（如 10.0.0.2）
3. [ ] 客户端 ping 服务端网关（10.0.0.1）通
4. [ ] 客户端 ping 公网 IP（如 8.8.8.8）通（验证 NAT 转发）
5. [ ] 服务端抓包（`tcpdump -i tun0`）确认上下行包均经过 TUN

## 连接稳定性

- [ ] 客户端休眠 / 合盖 10 分钟后唤醒，连接恢复或干净断开重连
- [ ] 客户端切换网络（Wi-Fi → 4G、4G → Wi-Fi），NAT rebinding 下连接保持
- [ ] 弱网（高延迟 / 丢包 5%）下，数据面仍可用（TCP 流不断超过 30 秒）
- [ ] 心跳超时触发后，虚拟 IP 正确释放（服务端日志确认）

## 顶替与会话

- [ ] 同一 username 两次连接，旧连接被顶替，新连接拿到 IP
- [ ] 被顶替的旧连接数据泵停止，无残留流量
- [ ] 断线重连后分配到空闲 IP（不要求同 IP）

## 数据面

- [ ] 大文件下载（> 1GB）完整传输，无损坏
- [ ] MTU = 1280 下，大包（接近 MTU）不超限
- [ ] 长连接 TCP 会话（SSH / mosh）持续 1 小时不断
- [ ] DNS 解析正常（通过 VPN 隧道）

## 安全

- [ ] 错误密码连接被拒（AuthFailed）
- [ ] 无效 CA 证书连接被拒（TLS 握手失败）
- [ ] 服务端无明文密码日志

## 可运维性

- [ ] 配置文件格式错误时报错信息可读
- [ ] NAT / IP forwarding 配置文档照着走能跑通
- [ ] `vpn hash-password`（或等价工具）能生成可用 argon2 hash

## 已知限制（V1，非 bug）

- 不支持同 username 多设备同时在线
- 不支持重连同 IP
- 不做动态 MTU 协商 / 分片
- 不自动配置服务端 NAT 规则（需手动）
