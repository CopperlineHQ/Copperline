# Builds the release wasm32 module and installs it as the committed artifact
# assets/zz9k/zz9k_plugin.wasm -- the blob a plain cargo build of the
# emulator embeds (rebuilding needs the wasm32-unknown-unknown rustup
# target). Mirrors crates/hostsocket-plugin/Makefile: the committed artifact
# is refreshed by this install step, never as a side effect of a build.

ARTIFACT = ../../assets/zz9k/zz9k_plugin.wasm

all:
	cargo build --release --locked --target wasm32-unknown-unknown
	install -m 0644 target/wasm32-unknown-unknown/release/zz9k_plugin.wasm $(ARTIFACT)

clean:
	cargo clean

.PHONY: all clean
