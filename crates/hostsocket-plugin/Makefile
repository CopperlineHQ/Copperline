# Builds the release wasm32 module and installs it as the committed artifact
# assets/hostsocket/hostsocket_plugin.wasm -- the blob a plain cargo build of
# the emulator embeds (rebuilding needs the wasm32-unknown-unknown rustup
# target; nothing else in the repo does). Mirrors guest/hostsocket/Makefile,
# which installs the companion stub ROM the same way: the committed artifact
# is refreshed by this install step, never as a side effect of a build.

ARTIFACT = ../../assets/hostsocket/hostsocket_plugin.wasm

all:
	cargo build --release --locked --target wasm32-unknown-unknown
	cp target/wasm32-unknown-unknown/release/hostsocket_plugin.wasm $(ARTIFACT)

clean:
	cargo clean

.PHONY: all clean
