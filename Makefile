cov:
	cargo llvm-cov --fail-under-lines 100 --ignore-filename-regex "(src/(main|server|client|data|tls|tun_setup)\.rs|examples/|tests/)"
