//! Q2 编译验证：确认 `TunSource` / `TunSink` newtype 正确实现数据面 trait。
//!
//! newtype 内部委托 `tun_rs::AsyncDevice` 的 `&self` 方法，逻辑机械，
//! 无需运行时 mock；真机收发验证见 `doc/release-test-checklist.md`。

fn assert_packet_source<T: vpn::data::PacketSource>() {}
fn assert_packet_sink<T: vpn::data::PacketSink>() {}

#[test]
fn test_tun_newtypes_implement_data_traits() {
    assert_packet_source::<vpn::server::TunSource>();
    assert_packet_sink::<vpn::server::TunSink>();
}
