.DEFAULT_GOAL := help

.PHONY: help fmt test build-cli

help:
	@printf '%s\n' \
		'Usage: make <target>' \
		'' \
		'Available targets:' \
		'  help          Show this help message (default target)' \
		'  fmt           Format all Rust code' \
		'  test          Run all default workspace tests with locked dependencies' \
		'  build-cli     Build the release CLI: target/release/ponydb'

fmt:
	cargo fmt --all

test:
	cargo test --locked --workspace

build-cli:
	cargo build --locked --release -p ponydb
