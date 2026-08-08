cov:
	cargo llvm-cov --fail-under-regions 100 --ignore-filename-regex "(src/(main|server|client|data)\.rs|examples/|tests/)"
