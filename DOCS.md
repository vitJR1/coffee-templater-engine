# coffee-core documentation

Reference for the core of coffee-templater: the template language, the wire protocol used by
wrappers, and the JSON document model. The original design specification (in Russian) is
[docs/core-spec.md](docs/core-spec.md).

- [Concepts](#concepts)
- [Template language](#template-language)
  - [Tags](#tags) · [Expressions](#expressions) · [How sections expand](#how-sections-expand)
  - [Loop locals](#loop-locals) · [Filters](#filters) · [Images](#images) · [Excel specifics](#excel-specifics)
  - [Inspecting templates](#inspecting-templates)
- [Data values](#data-values)
- [Options](#options)
- [Protocol](#protocol)
  - [Frames](#frames) · [Operations](#operations) · [Host callbacks](#host-callbacks) · [Errors](#errors)
- [ABI](#abi)
  - [WebAssembly](#webassembly) · [C ABI](#c-abi) · [Instances, threads and handles](#instances-threads-and-handles)
- [Limits](#limits)
- [Document model](#document-model)
- [Determinism](#determinism)
- [Versioning](#versioning)
- [Writing a wrapper](#writing-a-wrapper)
- [Differences from the TypeScript implementation](#differences-from-the-typescript-implementation)

---

## Concepts

- **Template:** an ordinary .docx/.xlsx/.pptx file containing tags such as `{{client.name}}`. It is
  **compiled** once into a handle and **rendered** many times with different data.
- **Data:** JSON plus a few tagged values for dates, exact decimals and binary data
  ([Data values](#data-values)).
- **Model:** a plain JSON representation of a document (paragraphs, runs, tables, cells, slides)
  that can be edited and **built** back into a file using the original as a base.
- **Host:** the program embedding the core. The core calls back into it for user-defined filters
  and optional formatting hooks.

The core is pure: it never touches the file system, network, clock or environment. Everything it
needs arrives in the request.

---

## Template language

Tags are written in the document text. Office often splits text into several runs (spell-check,
formatting, editing history), so `{{na` + `me}}` arrives as two pieces. The core joins the text of
each paragraph (or cell) before looking for tags, and the whole tag takes the formatting of the run
where it starts.

### Tags

With the default delimiters `{{` `}}`:

| Tag | Meaning |
| --- | --- |
| `{{expr}}` | prints a value |
| `{{%expr}}` | image ([Images](#images)) |
| `{{#expr}}…{{/expr}}` | section: loop over an array, enter an object, or render when truthy |
| `{{#each expr}}…{{/each}}` | loop over an array, or over an object's values with `@key` |
| `{{#if expr}}…{{/if}}` | render when truthy (empty arrays are falsy) |
| `{{#unless expr}}…{{/unless}}` | render when falsy |
| `{{^expr}}…{{/expr}}` | inverted section: render when falsy or an empty array |
| `{{else}}` | else branch of any section |
| `{{/}}` | closes the innermost section |
| `\{{text}}` | escaped delimiters, printed literally as `{{text}}` |
| `\\{{tag}}` | a literal backslash followed by a real tag |

A section opened with filters is closed by its path: `{{#items | sort: "name"}}…{{/items}}`.
`{{#if …}}`, `{{#unless …}}` and `{{#each …}}` close with `{{/if}}`, `{{/unless}}` and `{{/each}}`.

Delimiters, the escape character, sigils (`#`, `^`, `/`, `%`) and keywords (`else`, `if`, `unless`,
`each`) are configurable ([Options](#options)). For example, with
`{"syntax": {"section": "@", "close": "end ", "if": "когда", "else": "иначе"}}` you can write
`{{@когда ok}}да{{иначе}}нет{{end когда}}`.

### Expressions

```
path            client.name   items[0].title   items[-1]   map["key with spaces"]   this   .   ../title   @index
literal         "text"  'text'  “text”  «text»  42  -1.5  true  false  null
filters         value | upper | default: "—" | date: "DD.MM.YYYY"
arguments       literals or paths:  {{ a | default: b }}
```

- A path is resolved in the current scope first. If its first segment is missing there, the
  lookup walks up the enclosing scopes (Mustache-style), so outer variables are visible inside loops.
- `.` and `this` refer to the current item, `../` goes one scope up, and `@name` reads a loop local.
- Negative indexes count from the end. `length` works on arrays and strings.
- Only the data's own keys are reachable. Nothing like a prototype or internal member exists.
- Typographic quotes inserted by Word autocorrect (`“…”`, `„…“`, `«…»`, `‘…’`) are accepted.

### How sections expand

What a section repeats depends on where its opening and closing tags are:

| Placement | Repeats |
| --- | --- |
| both tags in the same paragraph or cell | the inline content between them |
| each tag alone in its own paragraph (Word, PowerPoint) | the paragraphs between them |
| tags in different cells of the same table row | the whole row |
| tags in different table rows | that range of rows |
| tags in different cells of the same sheet row (Excel) | the whole sheet row |

`{{else}}` must be at the same level as the section tags: inline, or alone in its own paragraph or
row. Other placements, such as a section that starts in a paragraph and ends inside a table, are
rejected with `invalid_section`.

### Loop locals

Inside loops:

| Local | Value |
| --- | --- |
| `@index` | 0-based position |
| `@number` | 1-based position |
| `@first`, `@last` | booleans |
| `@length` | number of items |
| `@key` | object key (`{{#each object}}`) |

```
{{#items}}{{@number}}. {{name}}{{^@last}}, {{/@last}}{{/items}}
```

### Filters

Arguments in brackets are optional; the default is shown after `=`. A filter declared by the host
with the same name overrides the built-in one.

**Strings**

| Filter | Description |
| --- | --- |
| `upper`, `lower` | case conversion |
| `capitalize` | first character upper-cased |
| `trim` | strips surrounding whitespace |
| `replace: search [, replacement = ""]` | replaces every occurrence |
| `truncate: [length = 50, suffix = "…"]` | shortens to `length` characters including the suffix |
| `prepend: text`, `append: text` | adds text, except to empty values |
| `default: [fallback = ""]` | fallback for `null`, missing, `""` or `[]` |

**Numbers and dates**

| Filter | Description |
| --- | --- |
| `number: [decimals = 0, thousands = " ", decimal = ","]` | `1234567.891 \| number: 2` → `1 234 567,89` |
| `fixed: [decimals = 2]` | `toFixed` |
| `round: [decimals = 0]` | rounds, returns a number |
| `currency: [code = "USD", locale = "en-US", decimals]` | `25 \| currency: "EUR", "de-DE"` → `25,00 €` |
| `percent: [decimals = 0]` | `0.256 \| percent: 1` → `25.6%` |
| `date: [format = "DD.MM.YYYY"]` | dates, ISO strings and timestamps (ms) |

Date format tokens: `YYYY YY MM M DD D HH H mm m ss s`. Text in `[brackets]` is printed literally:
`date: "[at] HH:mm"`.

`$decimal` values are rounded exactly by `number`, `fixed`, `round` and `currency` (half away
from zero, without going through floating point).

`currency` uses a built-in locale table: en, de, de-CH, ru, uk, be, kk, fr, es, it, pl, cs, sk, sv,
fi, nb, pt, pt-BR, nl, tr, ja, zh and ko. Unknown locales fall back to `en-US`. For full ICU
formatting, declare a host filter named `currency`.

**Collections**

| Filter | Description |
| --- | --- |
| `join: [separator = ", "]` | joins an array |
| `length` | length of an array, string or object |
| `first`, `last` | first / last item |
| `reverse` | reverses an array or string |
| `sort: [key, direction = "asc"]` | stable sort, optionally by a field; `"desc"` reverses |
| `where: key [, value]` | keeps items whose `key` equals `value`, or is truthy |
| `slice: [start = 0, end]` | like `Array.prototype.slice`, negative indexes allowed |
| `json: [indent = 0]` | `JSON.stringify` |

**Comparison and logic** (handy in conditions: `{{#if total | gt: 1000}}`)

| Filter | Description |
| --- | --- |
| `eq: other`, `ne: other` | equality (`1` equals `"1"`) |
| `gt`, `gte`, `lt`, `lte: other` | numeric comparison |
| `not`, `empty` | negation / emptiness check |
| `and: …values`, `or: …values` | all truthy / first truthy value |
| `contains: item` | array membership or substring |
| `yesno: [yes = "yes", no = "no"]` | `{{ paid \| yesno: "Paid", "Due" }}` |

### Images

`{{%logo}}` inserts a picture. The value is an object:

```json
{ "data": { "$blob": 0 }, "width": 120, "height": 60, "extension": "png", "alt": "Logo" }
```

- `data` is required: `{"$blob": n}`, `{"$base64": "…"}`, or a base64 / data-URL string.
- `width` and `height` are in pixels at 96 dpi. If one is missing, it is derived from the image's
  aspect ratio; if both are missing, the natural size is used.
- The format and natural size are detected for PNG, JPEG, GIF, BMP and SVG. Other formats need
  `extension`.
- `extension` must be 1–10 letters or digits (`png`, `.JPG` are fine).
- `null`, `false` or `""` removes the tag. In strict mode a missing value is an error.
- The same image (same bytes and extension) used many times, for example in a loop, is stored once
  in the package. New drawing ids continue after the highest id already present in the template.

In **Word** the picture is inline, at the tag position. In **PowerPoint** the tag must be the
content of a text box or placeholder: the shape is replaced by the picture, which is fitted
("contain") and centred into the shape's box. **Excel** does not support image tags yet
(`unsupported`).

### Excel specifics

- A cell holding only one tag whose value is a number, `$decimal` or boolean becomes a typed cell.
  Mixed text stays a string.
- Row loops renumber the rows below them. Copies of a looped row are contiguous; blank rows between
  template rows are kept.
- References follow the moved rows: formulas on every sheet (including `Sheet1!A5` and
  `'My sheet'!A5` from other sheets), defined names (print areas, named ranges), tables and their
  auto filters, the sheet's auto filter, merged ranges, conditional formatting, data validation and
  hyperlinks. Merges inside repeated rows are duplicated.
- Rows removed by an empty loop behave as if deleted in Excel: a reference to them becomes `#REF!`,
  and a range is clamped to its remaining rows (`#REF!` when none remain). Merges, validations,
  formatting and hyperlinks that covered only removed rows are dropped. Wrap totals over a loop that
  may be empty in `IFERROR(…, 0)`.
- A formula in a cell that receives a number or boolean is kept, with the value as its cached result.
- `calcChain.xml` is removed and `fullCalcOnLoad="1"` is set, so Excel recalculates on open.
- Shared strings containing tags are inlined into their cells.

### Inspecting templates

`inspect` returns:

```jsonc
{
  "format": "docx",
  "parts":  [{ "part": "word/document.xml", "tags": [ /* tag tree */ ] }],
  "tags":   [ /* tag tree of all parts */ ],
  "variables": ["number", "client.name", "items[].name", "items[].price"],
  "schema": { "client": { "name": "" }, "items": [{ "name": "", "price": "" }], "paid": true, "logo": null }
}
```

Tag tree nodes: `{type: "variable", path, expression, filters, raw}`,
`{type: "image", path, expression, raw}`, and
`{type: "section", mode, path, expression, raw, children, elseChildren?}`, where `mode` is
`section | inverted | if | unless | each`.

`schema` is a sample data object: strings for variables, arrays for loops, booleans for conditions
and `null` for images. Use it to build forms or validate input.

---

## Data values

Template data is ordinary JSON plus **tagged values**: objects with exactly one key from the table
below. An object with other or additional keys is a plain object.

| Tag | Meaning | Example |
| --- | --- | --- |
| `{"$date": "<ISO 8601>"}` | date / date-time | `{"$date": "2024-05-17"}`, `{"$date": "2024-05-17T10:00:00Z"}` |
| `{"$decimal": "<string>"}` | exact decimal | `{"$decimal": "1234.50"}` |
| `{"$blob": <index>}` | bytes from a blob of the same frame | images |
| `{"$base64": "<string>"}` | bytes from base64 or a data URL | for hosts where blobs are awkward |
| `{"$literal": <any>}` | the value as is, without tag parsing | `{"$literal": {"$date": "not a date"}}` |

- **Numbers** keep their source text: `12345678901234567890` and `1.50` print exactly as written.
  Arithmetic and filters use `f64`, except for `$decimal`.
- **Dates without a time zone** are *naive*, as in Excel, and print as written: `2024-05-17` or
  `2024-05-17T10:00:00.000`. Dates with a zone are normalised to UTC and print as
  `2024-05-17T07:00:00.000Z`. The core has no clock and no time zone database.
- An invalid tagged value (`{"$date": "nope"}`, an out-of-range blob index) is an `invalid_request`
  error.

Default printing: strings as is, `null`/missing as `""` (see `nullValue`), dates as ISO (see
`stringify.dateFormat`), arrays joined with `", "`, objects as JSON, booleans as `true`/`false`.

Suggested host conversions:

| Host | → JSON |
| --- | --- |
| Go | `time.Time` → `$date`; `[]byte` → `$blob`; `decimal.Decimal` / `*big.Rat` → `$decimal` |
| Python | `datetime` / `date` → `$date`; `bytes` → `$blob`; `Decimal` → `$decimal` |
| JS/TS | `Date` → `$date`; `Uint8Array` / `Buffer` → `$blob`; `bigint` → a number lexeme |

---

## Options

Passed to `compile` and `renderOnce`. Every field is optional. Unknown fields are ignored.

```jsonc
{
  "delimiters": { "start": "{{", "end": "}}" },   // or ["${", "}"]
  "escapeChar": "\\",                              // null disables escaping
  "syntax": { "section": "#", "inverted": "^", "close": "/", "image": "%",
              "else": "else", "if": "if", "unless": "unless", "each": "each" },
  "strict": false,                                 // missing values → missing_value error
  "linebreaks": true,                              // "\n" → line break, "\t" → tab (Word)
  "hostFilters": ["money", "phone"],               // filters implemented by the host
  "nullValue": { "mode": "empty" },                // empty | text (+ "text") | keep | host | error
  "stringify": { "mode": "default", "dateFormat": "iso" },  // mode: default | host; dateFormat: iso or a date pattern
  "limits": { "maxLoopIterations": 100000 }        // see Limits
}
```

- `hostFilters` are declared at compile time so that `unknown_filter` is reported immediately.
- `nullValue.mode`: `empty` prints `""`; `text` prints `nullValue.text`; `keep` prints the original
  tag; `host` asks the host; `error` behaves like `strict`.
- `stringify.mode: "host"` sends every non-string value to the host for formatting.

Empty or identical delimiters, an empty escape character, or an empty sigil give
`invalid_options`.

---

## Protocol

### Frames

Every request and response is one **frame**. All integers are `u32`, little-endian, unaligned.

```
offset  size   field
0       4      magic "CTF1" (0x43 0x54 0x46 0x31)
4       4      json_len
8       N      JSON (UTF-8, no BOM)
8+N     4      blob_count
...            blob_count × { u32 blob_len, bytes }
```

JSON refers to blobs by index: `{"$blob": 0}`.

**Request**

```json
{ "op": "render", "id": 17, "template": 3, "data": { "name": "Ann", "logo": { "data": { "$blob": 0 } } } }
```

`op` is required. `id` is optional and echoed back. Other fields depend on the operation.

**Response**

```json
{ "ok": true, "id": 17, "result": { "document": { "$blob": 0 } }, "warnings": [] }
{ "ok": false, "id": 17, "errors": [ { "code": "unclosed_section", "message": "Section is never closed", "part": "word/document.xml", "tag": "{{#items}}" } ] }
```

Response keys always come in this order, without extra whitespace. Wrappers must ignore unknown
fields.

### Operations

| Operation | Request fields | Result |
| --- | --- | --- |
| `info` | — | `coreVersion`, `abiVersion`, `modelVersion`, `formats`, `features` |
| `compile` | `template` (blob), `options?` | `{ template: <handle>, format, parts }` |
| `render` | `template` (handle), `data?` | `{ document: {"$blob": 0} }` |
| `renderOnce` | `template` (blob), `options?`, `data?` | `{ document: {"$blob": 0} }` |
| `inspect` | `template` (handle) | [inspect result](#inspecting-templates) |
| `dispose` | `template` (handle) | `{}`; unknown handles and repeated calls are not errors |
| `parse` | `document` (blob), `limits?` | `{ model }` |
| `build` | `model`, `base?` (blob), `limits?` | `{ document: {"$blob": 0} }` |
| `extractText` | `document` (blob), `limits?` | `{ text }` |
| `detect` | `document` (blob), `limits?` | `{ format }` |

Blob fields accept `{"$blob": n}` or `{"$base64": "…"}`. Blobs referenced by
`data` travel in the same request frame as the template, so in a `renderOnce` frame blob 0 is the
template and the data refers to blobs 1, 2, ….

`info` result:

```json
{
  "coreVersion": "0.1.0", "abiVersion": 1, "modelVersion": 1,
  "formats": ["docx", "xlsx", "pptx"],
  "features": { "images": ["docx", "pptx"], "slideLoops": false, "hostFilters": true }
}
```

Call `info` when the wrapper initialises and check that `abiVersion` is supported.

`build`: `base` is the original file. Styles, media and everything else not in the model come from
it. `base` is optional for docx/xlsx (a blank document is used) and required for pptx.

### Host callbacks

User filters, `nullValue: host` and `stringify: host` run on the host. The core sends a frame:

```json
{ "call": "filter", "name": "money", "value": 1250.5, "args": ["RUB"], "tag": { "raw": "{{total | money: \"RUB\"}}", "expression": "total | money: \"RUB\"", "part": "word/document.xml" } }
{ "call": "nullValue", "tag": { "raw": "{{client.phone}}", "expression": "client.phone", "part": "word/document.xml" } }
{ "call": "stringify", "value": { "$date": "2024-05-17" }, "tag": { … } }
```

Values use the [data notation](#data-values), including blobs in the callback frame. An object
that looks like a tag is wrapped in `$literal`. The host answers:

```json
{ "ok": true, "value": "1 250,50 ₽" }
{ "ok": false, "message": "unsupported currency" }
```

- Calls are synchronous. During a callback the host must not call operations of the same instance;
  only `ct_alloc` and `ct_free` are allowed.
- A filter may return any value: string, number, array or object. Filters also work in conditions
  and loops: `{{#each items | myFilter}}`.
- `ok: false` becomes `filter_failed` (or `host_error` for other calls), with the message in
  `details.hostMessage`. The wrapper should turn exceptions in user code into `ok: false`.
- A host without callback support returns 0 from the import. The core then reports `host_error`,
  but only if a callback is actually needed.

### Errors

```json
{
  "code": "unclosed_section",
  "message": "Section is never closed",
  "part": "word/document.xml",
  "tag": "{{#items}}",
  "details": {}
}
```

`part`, `tag`, `location` and `details` are optional. `compile` reports all problems it finds, not
just the first.

| Template codes | When |
| --- | --- |
| `unclosed_tag` | missing closing delimiter, or a tag spanning paragraphs/cells |
| `unclosed_section` | a section that is never closed |
| `unmatched_close` | a closing tag with no matching opening tag, or `else` outside a section |
| `invalid_expression` | syntax error inside a tag, or an empty tag |
| `unknown_filter` | a filter that is neither built in nor declared in `hostFilters` |
| `filter_failed` | a host filter returned `ok: false` |
| `missing_value` | strict mode, value missing |
| `invalid_section` | sections overlap or cross structure boundaries; misplaced `else` |
| `invalid_image` | the image value is not an image object, or the image tag is outside a shape (pptx) |
| `unsupported` | feature not available in this format (images in xlsx), malformed XML structure |

| Core codes | When |
| --- | --- |
| `invalid_request` | bad frame, unknown operation, invalid fields or tagged values |
| `invalid_options` | invalid options |
| `invalid_document` | not a zip, `[Content_Types].xml` missing, broken XML, unsafe zip entries |
| `unsupported_format` | not docx/xlsx/pptx |
| `invalid_model` | model does not match the schema, or refers to missing parts, slides or shapes |
| `unknown_template` | handle not found or already disposed |
| `limit_exceeded` | a [limit](#limits) was exceeded; `details.limit` names it |
| `host_error` | a host callback failed or answered badly |
| `internal` | a bug in the core; please open an issue |

Warnings use the same format and never stop an operation. v0.1 does not emit any yet.

---

## ABI

### WebAssembly

Module: `wasm32-unknown-unknown`, no WASI. Exports linear memory `memory`.

| Export | Purpose |
| --- | --- |
| `ct_abi_version() -> u32` | ABI version, currently `1` |
| `ct_alloc(len: u32) -> u32` | allocate `len` bytes; 0 on failure |
| `ct_free(ptr: u32, len: u32)` | free memory from `ct_alloc` or a `ct_call` result |
| `ct_call(req_ptr: u32, req_len: u32) -> u32` | run a request frame; returns a pointer to the response |

| Import (module `ct_host`) | Purpose |
| --- | --- |
| `call(req_ptr: u32, req_len: u32) -> u32` | host callback; return 0 if unsupported |

**Calling the core**

1. `ptr = ct_alloc(len)`, then copy the request frame to `memory[ptr..]`.
2. `res = ct_call(ptr, len)`, then `ct_free(ptr, len)`.
3. At `res` there is `[u32 frame_len][frame]`. Copy the frame, then `ct_free(res, frame_len + 4)`.

**Answering a callback**

1. Read the request frame at `memory[req_ptr..req_ptr + req_len]`.
2. Build the response frame and allocate `out = ct_alloc(frame_len + 4)`. The size must be exactly
   this, because the core frees the buffer as `ct_free(out, frame_len + 4)`.
3. Write `[u32 frame_len][frame]` at `out` and return `out`.

The import must always be provided. If no callbacks are used, a stub returning 0 is enough.
A complete host in about 100 lines of plain JavaScript is
[`conformance/run-wasm.mjs`](conformance/run-wasm.mjs).

### C ABI

```c
typedef struct { const uint8_t *ptr; size_t len; } ct_bytes;
typedef ct_bytes (*ct_host_fn)(void *user_data, ct_bytes request);  /* response from ct_alloc */

uint32_t ct_abi_version(void);
void    *ct_instance_new(ct_host_fn host, void *user_data);         /* host may be NULL */
void     ct_instance_free(void *instance);
ct_bytes ct_call(void *instance, ct_bytes request);                 /* free with ct_free */
uint8_t *ct_alloc(size_t len);
void     ct_free(uint8_t *ptr, size_t len);
```

Frames and semantics are the same as in WebAssembly, without the length prefix: lengths travel in
`ct_bytes`. The instance is explicit, and callbacks are bound to it through `user_data`. A host
callback returns a frame allocated with `ct_alloc`, which the core frees, or `{NULL, 0}` if
callbacks are not supported. Header: [`crates/capi/include/coffee_core.h`](crates/capi/include/coffee_core.h).

### Instances, threads and handles

- An instance is **single-threaded**. Wrappers get parallelism from a pool of instances (Go: a
  compiled module instantiated per pool slot; Python: one instance per thread).
- Template handles belong to one instance. A wrapper's `Template` object should keep the source
  bytes and options and compile lazily in each instance it runs on, caching the handle per instance.
- Always `dispose` templates: Go `Close()`, Python context managers or finalizers, JS
  `FinalizationRegistry` plus an explicit `dispose()`.
- For cancellation and timeouts, interrupt the WebAssembly instance (wazero
  `WithCloseOnContextDone`, wasmtime epoch interruption) and drop it from the pool.
- A panic in the native build becomes an `internal` error, and the instance keeps working.
- In WebAssembly a panic traps. A trap (including a host exception thrown from a callback) is
  detected on the next `ct_call`: it answers `internal` once, resets the instance and drops its
  templates. Wrappers should then recompile templates lazily, or simply replace the instance.
  `ct_call` from inside a callback is answered with `invalid_request`.

---

## Limits

Set through `options.limits` (compile) or the `limits` field (parse, build, extractText, detect).
`0` means no limit.

| Limit | Default | Meaning |
| --- | --- | --- |
| `maxInputBytes` | 100 MiB | input file size |
| `maxEntries` | 10 000 | zip entries |
| `maxUncompressedBytes` | 500 MiB | total uncompressed size |
| `maxCompressionRatio` | 200 | uncompressed / compressed, per entry larger than 1 MiB |
| `maxXmlDepth` | 256 | XML nesting depth |
| `maxOutputBytes` | 500 MiB | result size before compression |
| `maxLoopIterations` | 1 000 000 | total loop iterations per render |
| `maxHostCalls` | 1 000 000 | callbacks per operation |

Always on, not configurable:

- `<!DOCTYPE>` and external entities are never expanded. Only the five predefined entities and
  numeric references are decoded.
- Zip entries with absolute paths, `..`, backslashes, duplicate names, encryption, ZIP64 or a bad
  CRC are rejected (`invalid_document`).
- Sizes are checked against the declared sizes before inflating, and inflation is capped.
- XML nesting is capped at 1 024 levels even when `maxXmlDepth` is 0 or larger.
- Template sections nest at most 128 levels deep (`limit_exceeded`, `details.limit = "sectionDepth"`).
- JSON nesting in requests is limited to 128 levels.
- Package entry names are validated when writing, too.

---

## Document model

`parse` turns a file into a JSON model. Edit it and pass it to `build` together with the original
file. Properties the model doesn't cover are kept as raw XML (`props`, `raw`), and convenient
fields are written back only when they change, so a parse → build round trip doesn't lose
formatting. Every model carries `format` and `modelVersion`. The full JSON Schema is
[`schema/model.schema.json`](schema/model.schema.json).

### Word: `format: "docx"`

```jsonc
{
  "format": "docx", "modelVersion": 1,
  "parts": [{
    "path": "word/document.xml", "kind": "body",            // body | header | footer
    "sectionProps": "<w:sectPr>…</w:sectPr>",
    "blocks": [
      { "type": "paragraph", "style": "Heading1", "align": "center", "list": { "id": 1, "level": 0 },
        "children": [
          { "type": "run", "text": "Hello\nworld\t!", "bold": true, "italic": true, "underline": true,
            "strike": false, "color": "FF0000", "highlight": "yellow", "size": 14, "font": "Arial",
            "vertAlign": "superscript", "style": "Strong", "props": "<w:rPr>…</w:rPr>" },
          { "type": "break", "kind": "page" },              // page | column
          { "type": "hyperlink", "url": "https://example.com", "anchor": "bookmark", "children": [ … ] },
          { "type": "raw", "xml": "<w:bookmarkStart …/>" }
        ],
        "props": "<w:pPr>…</w:pPr>" },
      { "type": "table", "props": "…", "grid": "…",
        "rows": [{ "props": "…", "cells": [{ "colSpan": 2, "vMerge": "restart", "props": "…", "blocks": [ … ] }] }] },
      { "type": "raw", "xml": "<w:sdt>…</w:sdt>" }
    ]
  }]
}
```

In run text, `\n` is a line break, `\t` a tab, `U+2011` a non-breaking hyphen and `U+00AD` a soft
hyphen. Neighbouring runs with identical formatting are merged on parse.

### Excel: `format: "xlsx"`

```jsonc
{
  "format": "xlsx", "modelVersion": 1,
  "sheets": [{
    "name": "Report", "path": "xl/worksheets/sheet1.xml",   // omit path to create a new sheet
    "merges": ["A1:D1"],
    "rows": [{ "index": 1, "height": 20, "hidden": false,
      "cells": [
        { "ref": "A1", "value": "Name", "style": 1 },
        { "ref": "B1", "value": 42 },
        { "ref": "C1", "value": true },
        { "ref": "D1", "value": "2024-05-17", "type": "date" },
        { "ref": "E1", "value": 84, "formula": "B1*2" },
        { "ref": "F1", "value": "#N/A", "type": "error" },
        { "ref": "G1", "value": "Bold part", "richText": "<r>…</r>" }
      ] }]
  }]
}
```

- `value` is a string, number, boolean or `null`.
- Date-formatted numbers come as ISO strings with `type: "date"`, and are written back as serial
  numbers with a date style.
- Shared formulas are expanded per cell. `formula` has no leading `=`.
- Sheets missing from the model are removed. Sheets can be renamed, reordered and created.
- `ref` may be omitted: the cell then goes to the column after the previous one.

### PowerPoint: `format: "pptx"`

```jsonc
{
  "format": "pptx", "modelVersion": 1,
  "slides": [{
    "path": "ppt/slides/slide1.xml",
    "notes": "Speaker notes",
    "shapes": [
      { "type": "text", "id": 2, "name": "Title 1", "placeholder": "title",
        "paragraphs": [{ "align": "center", "level": 0, "props": "…", "endProps": "…",
          "children": [ { "type": "run", "text": "Hello", "bold": true, "italic": false, "underline": true,
                          "strike": false, "size": 40, "color": "00FF00", "font": "Arial", "props": "…" },
                        { "type": "raw", "xml": "<a:fld …>…</a:fld>" } ] }] },
      { "type": "table", "id": 4, "name": "Table 3", "rows": [{ "cells": [{ "paragraphs": [ … ] }] }] },
      { "type": "picture", "id": 5, "name": "Picture 4" }      // also group | connector | graphic | other
    ]
  }]
}
```

- Shapes are matched by `id` on build. Shapes missing from the list are removed, and reordering
  the list changes the z-order. To insert a new shape, pass `xml` on a non-text shape.
- Slides can be reordered and removed; duplicating a slide is not supported. `base` is required.
- Table rows can be added: new rows copy the formatting of the last row.

### Plain text

`extractText` returns paragraphs (docx body), tab-separated cells with a sheet-name header (xlsx),
or `--- Slide N ---` blocks with notes (pptx).

---

## Determinism

The same input produces the same bytes on every host:

- zip entries keep their input order, and new entries (images, sheets) are appended in creation
  order;
- every entry has the timestamp `1980-01-01 00:00:00`;
- deflate level 6 by the implementation bundled in the module; entries that don't compress are
  stored;
- media names (`image_ct1.png`) and relationship ids come from counters, never from randomness;
- JSON responses have a fixed key order and no extra whitespace.

The conformance suite pins the `sha256` of rendered documents. Native builds and WebAssembly
produce the same hashes.

---

## Versioning

| Version | Where | Bumped when |
| --- | --- | --- |
| `abiVersion` | `ct_abi_version`, `info` | exports, imports or the frame format change (breaking) |
| `coreVersion` | `info` | semver of the core |
| `modelVersion` | the model and `info` | breaking changes to the JSON model |

New operations, new optional request or response fields, and new error or warning codes are
**not** breaking. `build` accepts the current and the previous `modelVersion`.

JSON Schema for the model, limits and errors is generated from the Rust types (`make schema`), so
wrappers can generate their own types from it.

---

## Writing a wrapper

1. **Load** `coffee_core.wasm` with your runtime (Go: wazero; Python: wasmtime; JS: `WebAssembly`).
   Provide the `ct_host.call` import.
2. **Check** `info.abiVersion` once at start-up.
3. **Frames:** implement encode/decode (about 40 lines) and the call sequence from
   [WebAssembly](#webassembly).
4. **Values:** convert native values to the tagged notation. Pass raw bytes as blobs, not base64.
   Keep large integers and decimals as number lexemes or `$decimal`.
5. **Filters:** take `filters: {name: fn}` in the native API, send their names in
   `options.hostFilters`, and dispatch `call: "filter"` frames to them. Convert arguments to native
   types and return any value. Catch exceptions and answer `ok: false`.
6. **Errors:** map `errors[]` to a native error type keeping `code`, `part`, `tag` and `details`
   (Go: `*coffee.Error` with `errors.Is` on the code; Python: a `TemplateError` hierarchy).
7. **Concurrency:** pool instances, compile templates lazily per instance, and always `dispose`.
8. **Verify:** run every case in [`conformance/`](conformance/README.md) through the wrapper. All
   `sha256` values must match.

Suggested API shape, for consistency between languages:

| Capability | Go | Python | JS/TS |
| --- | --- | --- | --- |
| Create | `coffee.New(coffee.Options{…})` | `Templater(delimiters=("{{","}}"), filters={…})` | `createTemplater({…})` |
| Compile | `t.Compile(tpl []byte) (*Template, error)` | `t.compile(data: bytes) -> Template` | `t.compile(bytes)` |
| Render | `tpl.Render(ctx, data any) ([]byte, error)` | `tpl.render(data) -> bytes` | `tpl.render(data)` |
| Inspect | `tpl.Inspect() (*InspectResult, error)` | `tpl.inspect()` | `tpl.inspect()` |
| Release | `tpl.Close()` | `with t.compile(…) as tpl:` | `tpl.dispose()` |
| Model | `coffee.Parse(doc)` / `coffee.Build(model, base)` | `parse(doc)` / `build(model, base=…)` | `parse` / `build` |

---

## Differences from the TypeScript implementation

The template semantics match the TS version: rendered documents in the conformance suite are
identical to it part for part. Intentional differences:

- `compile` reports all errors instead of throwing the first one. Expression and filter errors in a
  part stop the structural checks of that part only, so unclosed sections there are reported once
  the expressions are fixed.
- There is no local time zone. Naive dates print as written, and the `utc` argument of `date` is
  ignored. In TS, formatting depended on the machine's time zone.
- Number lexemes from JSON are kept (`1.50` prints `1.50`).
- `$decimal`, `$blob`, `$base64` and `$literal` values; `nullValue` and `stringify` modes instead of
  JS callbacks; host filters instead of JS functions.
- `currency` uses the built-in locale table instead of `Intl`.
- `length`, `truncate`, `slice` and `capitalize` count Unicode characters, not UTF-16 code units.
  The difference shows only for characters outside the BMP, such as emoji.
- Unsafe zip entries are rejected. TS silently stripped a leading `/`.
- Excel: a self-closing `<f t="shared" …/>` no longer merges with the next formula during row
  renumbering, which corrupted formulas in TS.
- Excel: references to rows removed by an empty loop become `#REF!` (TS left them pointing at
  whatever row moved into their place); copies of a looped row after a blank row are contiguous;
  tables, defined names, auto filters and references from other sheets are updated; a formula in a
  cell receiving a scalar value is kept (TS dropped it).
- Images: identical images are stored once; extensions are validated; drawing ids don't collide
  with ids already in the template.
