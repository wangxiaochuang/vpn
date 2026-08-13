fn assert_packet_source<T: vpn_core::data::PacketSource>() {}
fn assert_packet_sink<T: vpn_core::data::PacketSink>() {}

#[test]
fn test_tun_newtype_implements_data_traits() {
    assert_packet_source::<vpn_core::data::Tun>();
    assert_packet_sink::<vpn_core::data::Tun>();
}
