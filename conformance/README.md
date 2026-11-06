# Conformance-набор

Языконезависимые кейсы (спецификация §11). Их прогоняют ядро (`cargo test -p coffee-abi --test
conformance`) и каждая обёртка; эталонный раннер для WebAssembly — `node conformance/run-wasm.mjs`.

```
fixtures/            шаблоны: generate.py (python-docx, openpyxl, python-pptx), make_broken.py
cases/<name>/
  case.json          описание кейса
  data.json          данные (необязательно, иначе поле "data" в case.json)
  blobs/             блобы для "$blob": индекс = позиция файла в отсортированном списке
```

## case.json

```jsonc
{
  "description": "…",
  "op": "render",                       // info | compile | render | inspect | parse | extractText | detect
  "template": "fixtures/invoice.docx",  // путь от conformance/; для parse/extractText/detect это документ
  "options": { "strict": true },        // options для compile
  "limits": { "maxEntries": 3 },        // limits для parse/extractText/detect
  "hostFilters": {                      // имена автоматически попадают в options.hostFilters
    "money": { "type": "format", "pattern": "{value} {arg0}" },   // String(value), String(args[i])
    "shout": { "type": "upper" },                                  // String(value).toUpperCase()
    "boom":  { "type": "throw", "message": "no" }                  // { ok: false, message }
  },
  "data": { … },
  "expect": {
    "errors":  [{ "code": "unclosed_section", "tag": "{{#items}}" }],  // по порядку, подмножество полей
    "result":  [{ "path": "variables", "equals": […] }],               // ответ операции
    "text": "…", "textContains": ["…"],                                 // через extractText
    "model":   [{ "path": "sheets[0].rows[2].cells[1]", "equals": { "value": 2 } }],  // через parse
    "parts":   { "word/document.xml": { "contains": ["<w:br/>"], "notContains": ["{{"] } },
    "files":   { "contains": ["word/media/image_ct1.png"], "notContains": [] },
    "sha256":  "…"                                                      // документ целиком
  }
}
```

Для `render` и `inspect` раннер сначала вызывает `compile`, потом операцию, потом `dispose`.

`equals` сравнивается как подмножество: у объектов проверяются только перечисленные ключи, у
массивов совпадают длина и каждый элемент. Проверки идут от устойчивых к строгим:
`text` → `model` → `parts` → `sha256`. Благодаря детерминизму (§12) `sha256` одинаков на всех
хостах и ловит любые расхождения обёрток. Если ядро меняет вывод намеренно, хэши обновляются
командой `CONFORMANCE_PRINT=1 cargo test -p coffee-abi --test conformance -- --nocapture`.
