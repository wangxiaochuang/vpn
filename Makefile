server:
	RUST_LOG=debug cargo run -p vpn-server -- --config server.toml

client:
	sudo RUST_LOG=debug cargo run -p vpn-client -- --config client.toml

cov:
	cargo llvm-cov --fail-under-lines 100 --ignore-filename-regex "(vpn-(core|server|client)/src/(main|server|client|data|tun_setup)\.rs|vpn-(core|server|client)/examples/|vpn-(server|client|tests)/tests/|xtask/src/main.rs)"
