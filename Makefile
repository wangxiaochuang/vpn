server:
	RUST_LOG=debug cargo run -p vpn --bin vpn -- server --config server.toml

client:
	RUST_LOG=debug sudo cargo run -p vpn --bin vpn -- client --config client.toml

cov:
	cargo llvm-cov --fail-under-lines 100 --ignore-filename-regex "(vpn/src/(main|server|client|data|tls|tun_setup)\.rs|vpn/examples/|vpn/tests/|xtask/src/main.rs)"
