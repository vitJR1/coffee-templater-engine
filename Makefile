WASM_TARGET := wasm32-unknown-unknown
WASM_OUT    := target/$(WASM_TARGET)/release/coffee_core_wasm.wasm

.PHONY: test wasm conformance capi schema lint

test:
	cargo test --workspace
	cargo test -p coffee-core --features schema --test schema

wasm:
	cargo build -p coffee-wasm --release --target $(WASM_TARGET)
	mkdir -p dist
	cp $(WASM_OUT) dist/coffee_core.wasm

conformance: wasm
	node conformance/run-wasm.mjs dist/coffee_core.wasm
	node conformance/trap-recovery.mjs dist/coffee_core.wasm

capi:
	cargo build -p coffee-capi --release

schema:
	SCHEMA_WRITE=1 cargo test -p coffee-core --features schema --test schema

lint:
	cargo clippy --workspace --all-targets -- -D warnings
	cargo fmt --all -- --check
