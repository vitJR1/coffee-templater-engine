# coffee-core

The shared Rust core of **coffee-templater**: a templating engine for **Word (.docx)**, **Excel (.xlsx)**
and **PowerPoint (.pptx)**, plus a JSON document model to parse Office files and build them back.

It ships as a single **`coffee_core.wasm`** file that every language wrapper (Go, Python, JS, …)
embeds, or as a native library with a C ABI. All logic lives in the core, so a document rendered
from Go is byte-for-byte identical to the same document rendered from Python.

- Variables, loops, conditions, `else`, filters and images, all MIT-licensed
- Tags split by Word into several runs (`{{na` + `me}}`) are handled transparently
- Loops repeat inline text, whole paragraphs, table rows or spreadsheet rows
- Excel: numbers stay numbers, rows below a loop shift, formulas and merged cells are adjusted
- `inspect` lists template variables and returns a sample data shape
- `parse` → plain JSON model → `build`: edit documents without losing formatting
- Custom filters implemented by the host language (`{{ price | money: "EUR" }}`)
- **Pure and safe:** no I/O, no clock, no environment. Inputs are treated as untrusted: zip-bomb,
  path-traversal, XML depth and output-size limits are on by default, and DTDs are never expanded.
- **Deterministic:** the same input gives the same bytes on every host and platform

> **Status:** v0.1, pre-release. The template semantics are a faithful port of the TypeScript
> implementation `coffee-templater`: rendered documents in the conformance suite match it part for
> part. The ABI is versioned (`abiVersion = 1`) but may still change before 1.0.

## How it fits together

```
 your app ──► wrapper (Go / Python / JS) ──► coffee_core.wasm
              idiomatic API, value conversion      templating, document model, OPC/ZIP/XML
              user filters (callbacks)  ◄────────  host callbacks
```

Wrappers exchange **frames** with the core: one JSON header plus binary blobs, so documents and
images are never base64-encoded. One exported function handles every operation.
See [DOCS.md](DOCS.md) for the full reference.

## Quick start

### From Rust

```toml
[dependencies]
coffee-core = { git = "https://github.com/<owner>/coffee-templater-engine" }
```

```rust
use coffee_core::{value::parse_data, NoHost, Options, Template};

let template = Template::compile(&std::fs::read("invoice.docx")?, Options::default())?;
let data = parse_data(r#"{ "number": 42, "client": { "name": "Ann" }, "items": [] }"#, &[])?;
let docx = template.render(&data, &mut NoHost)?;
std::fs::write("invoice-42.docx", docx)?;
```

### From any language via WebAssembly

```js
// Node.js, no dependencies. callCore() is the ~30-line frame helper from conformance/run-wasm.mjs
const { instance } = await WebAssembly.instantiate(wasmBytes, { ct_host: { call: () => 0 } })
const response = callCore(instance, { op: 'renderOnce', template: { $blob: 0 }, data: { number: 42 } }, [docxBytes])
// response.json → { ok: true, result: { document: { $blob: 0 } } }, response.blobs[0] → the .docx
```

### From C / native code

```c
#include "coffee_core.h"

void *core = ct_instance_new(NULL, NULL);          /* or a host callback for custom filters */
ct_bytes response = ct_call(core, request_frame);  /* same frames as WebAssembly */
ct_free((uint8_t *)response.ptr, response.len);
ct_instance_free(core);
```

## Template syntax at a glance

| Syntax | Meaning |
| --- | --- |
| `{{client.name}}`, `{{items[0].title}}` | variable / path |
| `{{price \| number: 2}}` | filter with arguments |
| `{{#items}}…{{/items}}` | loop over an array, context object or truthy check |
| `{{#each prices}}{{@key}}={{.}}{{/each}}` | explicit loop (arrays and objects) |
| `{{#if paid}}…{{else}}…{{/if}}`, `{{#unless …}}`, `{{^items}}` | conditions |
| `{{%logo}}` | image |
| `\{{literal}}` | escaped delimiters |

Delimiters and sigils are configurable (`${x}`, `[[x]]`, `<<x>>`, …).
36 built-in filters, loop locals (`@index`, `@first`, …) and the expansion rules are documented in
[DOCS.md](DOCS.md#template-language).

## Repository layout

```
crates/core      templating engine, document model, OPC/ZIP/XML (no ABI concerns)
crates/abi       frames, operation dispatcher, host callbacks
crates/wasm      cdylib → coffee_core.wasm (wasm32-unknown-unknown, no WASI)
crates/capi      cdylib/staticlib + include/coffee_core.h
schema/          JSON Schema of the document model, limits and errors (generated)
conformance/     language-independent test cases + a Node.js runner for the .wasm
docs/            design specification (Russian)
```

## Building

Requires Rust ≥ 1.87 with the `wasm32-unknown-unknown` target, and Node.js ≥ 18 for the WebAssembly
conformance run.

```sh
rustup target add wasm32-unknown-unknown

make test         # all Rust tests: unit, ported TS tests, ABI, conformance, schema freshness
make wasm         # → dist/coffee_core.wasm
make conformance  # run the conformance cases against dist/coffee_core.wasm in Node.js
make capi         # → target/release/libcoffee_core_capi.{a,so,dylib}
make schema       # regenerate schema/*.json
make lint         # clippy + rustfmt
cargo run -p coffee-core --release --example bench -- 10000   # render benchmark
```

Every push to `main` builds the package in GitHub Actions (`package` workflow):
`coffee_core.wasm` with the JSON schemas, plus C ABI libraries and the header for Linux, macOS and
Windows. They are published as the rolling `nightly` pre-release. Pushing a `v*` tag publishes a versioned
GitHub Release with the same files.

## Writing a wrapper

A wrapper is thin. It loads `coffee_core.wasm` (wazero, wasmtime, `WebAssembly`), converts native
values to the tagged JSON notation (`{"$date": …}`, `{"$decimal": …}`, `{"$blob": n}`), answers
callbacks for user filters and maps errors to native exceptions. The
[conformance suite](conformance/README.md) is the contract: run every case through your wrapper and
check that the `sha256` hashes match. A step-by-step guide is in
[DOCS.md](DOCS.md#writing-a-wrapper).

## Contributing

Issues and pull requests are welcome. Before submitting:

- `make lint test conformance` passes;
- behaviour changes come with a test, and with a conformance case if they are visible to wrappers;
- intentional output changes update the pinned hashes
  (`CONFORMANCE_PRINT=1 cargo test -p coffee-abi --test conformance -- --nocapture`).

## License

[MIT](LICENSE)
