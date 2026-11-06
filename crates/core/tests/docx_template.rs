//! Port of `test/docx-template.test.ts` from the TS implementation.

mod common;
use coffee_core::host::FnHost;
use coffee_core::{ErrorCode, Options, Value};
use common::*;

// ------------------------------------------------------------------------------- variables

#[test]
fn replaces_a_tag_split_across_runs() {
    assert_eq!(body(&render(&docx(&p(&["Hello, ", "{{na", "me}}", "!"])), r#"{"name":"World"}"#)), "Hello, World!");
}

#[test]
fn escapes_xml_special_characters_in_values() {
    let out = render(&docx(&p(&["{{v}}"])), r#"{"v":"<b> & \"q\""}"#);
    assert!(read_part(&out, "word/document.xml").contains(r#"&lt;b&gt; &amp; "q""#));
}

#[test]
fn supports_nested_paths_indexes_and_scope_lookup() {
    let out = render(
        &docx(&p(&[r#"{{a.b.c}} {{list[1]}} {{list[-1]}} {{obj["key with space"]}}"#])),
        r#"{"a":{"b":{"c":1}},"list":["x","y","z"],"obj":{"key with space":"ok"}}"#,
    );
    assert_eq!(body(&out), "1 y z ok");
}

#[test]
fn does_not_expose_prototype_properties() {
    let out = render(&docx(&p(&["[{{constructor}}][{{a.__proto__}}][{{s.length}}]"])), r#"{"a":{},"s":"abc"}"#);
    assert_eq!(body(&out), "[][][3]");
}

#[test]
fn prints_missing_values_via_null_value() {
    assert_eq!(body(&render(&docx(&p(&["[{{missing}}]"])), "{}")), "[]");
    let mut host = FnHost::new().on_null(|tag| Value::string(format!("<{}?>", tag.expression)));
    let out = render_host(&docx(&p(&["{{missing}}"])), "{}", opts(r#"{"nullValue":{"mode":"host"}}"#), &mut host).unwrap();
    assert_eq!(body(&out), "<missing?>");
    assert_eq!(body(&render_with(&docx(&p(&["{{missing}}"])), "{}", opts(r#"{"nullValue":{"mode":"text","text":"—"}}"#))), "—");
    assert_eq!(body(&render_with(&docx(&p(&["{{missing}}"])), "{}", opts(r#"{"nullValue":{"mode":"keep"}}"#))), "{{missing}}");
}

#[test]
fn throws_on_missing_values_in_strict_mode() {
    let tpl = compile(&docx(&p(&["{{missing}}"])), opts(r#"{"strict":true}"#)).unwrap();
    let err = tpl.render(&data("{}"), &mut coffee_core::NoHost).unwrap_err();
    assert_eq!(err.code, ErrorCode::MissingValue);
    assert!(err.message.contains("Missing value"));
    let tpl = compile(&docx(&p(&["{{missing}}"])), opts(r#"{"nullValue":{"mode":"error"}}"#)).unwrap();
    assert_eq!(tpl.render(&data("{}"), &mut coffee_core::NoHost).unwrap_err().code, ErrorCode::MissingValue);
}

#[test]
fn converts_newlines_and_tabs_into_word_breaks() {
    let xml = read_part(&render(&docx(&p(&["{{v}}"])), r#"{"v":"a\nb\tc"}"#), "word/document.xml");
    assert!(xml.contains(r#"a</w:t><w:br/><w:t xml:space="preserve">b</w:t><w:tab/><w:t xml:space="preserve">c"#));
    let off = render_with(&docx(&p(&["{{v}}"])), r#"{"v":"a\nb"}"#, opts(r#"{"linebreaks":false}"#));
    assert!(!read_part(&off, "word/document.xml").contains("<w:br/>"));
}

#[test]
fn adds_xml_space_preserve_to_text_that_receives_values() {
    let tpl = docx("<w:p><w:r><w:t>{{v}}</w:t></w:r></w:p>");
    assert!(
        read_part(&render(&tpl, r#"{"v":"  padded  "}"#), "word/document.xml").contains(r#"<w:t xml:space="preserve">  padded  </w:t>"#)
    );
}

#[test]
fn stringifies_numbers_booleans_dates_and_arrays() {
    let out = render(&docx(&p(&["{{n}}|{{b}}|{{d}}|{{arr}}"])), r#"{"n":1.5,"b":false,"d":{"$date":"2024-01-02T03:04:05Z"},"arr":[1,2]}"#);
    assert_eq!(body(&out), "1.5|false|2024-01-02T03:04:05.000Z|1, 2");
}

#[test]
fn keeps_number_lexemes_and_formats_dates_with_options() {
    let out = render(&docx(&p(&["{{big}}|{{price}}|{{m}}"])), r#"{"big":12345678901234567890,"price":1.50,"m":{"$decimal":"1234.50"}}"#);
    assert_eq!(body(&out), "12345678901234567890|1.50|1234.50");
    let out = render_with(&docx(&p(&["{{d}}"])), r#"{"d":{"$date":"2024-05-17"}}"#, opts(r#"{"stringify":{"dateFormat":"DD.MM.YYYY"}}"#));
    assert_eq!(body(&out), "17.05.2024");
}

// ------------------------------------------------------------------------------- escaping

#[test]
fn prints_escaped_delimiters_literally() {
    assert_eq!(body(&render(&docx(&p(&[r"\{{name}} = {{name}}"])), r#"{"name":"x"}"#)), "{{name}} = x");
}

#[test]
fn keeps_a_literal_backslash_before_a_real_tag_with_a_double_escape() {
    assert_eq!(body(&render(&docx(&p(&[r"C:\\{{dir}}"])), r#"{"dir":"tmp"}"#)), r"C:\tmp");
}

#[test]
fn leaves_other_backslashes_untouched() {
    assert_eq!(body(&render(&docx(&p(&[r"a\b {{x}}"])), r#"{"x":1}"#)), r"a\b 1");
}

#[test]
fn supports_a_custom_escape_character_or_disabling_it() {
    assert_eq!(body(&render_with(&docx(&p(&["!{{a}} {{a}}"])), r#"{"a":1}"#, opts(r#"{"escapeChar":"!"}"#))), "{{a}} 1");
    assert_eq!(body(&render_with(&docx(&p(&[r"\{{a}}"])), r#"{"a":1}"#, opts(r#"{"escapeChar":null}"#))), r"\1");
}

// ------------------------------------------------------------------------------- delimiters and syntax

#[test]
fn uses_dollar_brace_delimiters() {
    let o = opts(r#"{"delimiters":["${","}"]}"#);
    assert_eq!(body(&render_with(&docx(&p(&["Hi ${name}, {{untouched}}"])), r#"{"name":"Ann"}"#, o)), "Hi Ann, {{untouched}}");
}

#[test]
fn uses_delimiters_with_xml_special_characters() {
    let o = opts(r#"{"delimiters":{"start":"<<","end":">>"}}"#);
    assert_eq!(body(&render_with(&docx(&p(&["Hi &lt;&lt;na", "me&gt;&gt;"])), r#"{"name":"Ann"}"#, o)), "Hi Ann");
}

#[test]
fn uses_single_brace_delimiters() {
    let o = opts(r#"{"delimiters":["{","}"]}"#);
    assert_eq!(body(&render_with(&docx(&p(&["{#items}{.};{/items}"])), r#"{"items":[1,2]}"#, o)), "1;2;");
}

#[test]
fn allows_overriding_sigils_and_keywords() {
    let o = opts(r#"{"syntax":{"section":"@","close":"end ","if":"когда","else":"иначе"}}"#);
    assert_eq!(body(&render_with(&docx(&p(&["{{@когда ok}}да{{иначе}}нет{{end когда}}"])), r#"{"ok":false}"#, o)), "нет");
}

#[test]
fn rejects_invalid_options() {
    for bad in [r#"{"delimiters":["{{","{{"]}"#, r#"{"delimiters":["","}}"]}"#, r#"{"escapeChar":""}"#, r#"{"nullValue":{"mode":"nope"}}"#]
    {
        let e = Options::from_json(Some(&serde_json::from_str(bad).unwrap())).unwrap_err();
        assert_eq!(e.code, ErrorCode::InvalidOptions, "{bad}");
    }
}

// ------------------------------------------------------------------------------- sections

#[test]
fn repeats_paragraphs_when_section_tags_are_alone_in_their_paragraphs() {
    let tpl = docx(&[p(&["Before"]), p(&["{{#items}}"]), p(&["- {{name}}"]), p(&["{{/items}}"]), p(&["After"])].concat());
    assert_eq!(body(&render(&tpl, r#"{"items":[{"name":"a"},{"name":"b"}]}"#)), "Before\n- a\n- b\nAfter");
    assert_eq!(body(&render(&tpl, r#"{"items":[]}"#)), "Before\nAfter");
}

#[test]
fn repeats_inline_content_within_a_paragraph() {
    assert_eq!(body(&render(&docx(&p(&["Tags: {{#tags}}", "[{{.}}]", "{{/tags}}."])), r#"{"tags":["x","y"]}"#)), "Tags: [x][y].");
}

fn cell(s: &str) -> String {
    format!("<w:tc><w:p><w:r><w:t>{s}</w:t></w:r></w:p></w:tc>")
}

#[test]
fn repeats_table_rows() {
    let tpl = docx(&format!(
        "<w:tbl><w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>",
        cell("Name"),
        cell("Qty"),
        cell("{{#rows}}{{name}}"),
        cell("{{qty}}{{/rows}}")
    ));
    let xml = read_part(&render(&tpl, r#"{"rows":[{"name":"a","qty":1},{"name":"b","qty":2}]}"#), "word/document.xml");
    assert_eq!(xml.matches("<w:tr>").count(), 3);
    assert!(!xml.contains("{{"));
    assert_eq!(body(&render(&tpl, r#"{"rows":[]}"#)), "Name\nQty");
}

#[test]
fn repeats_multiple_rows() {
    let row = |s: &str| format!("<w:tr><w:tc><w:p><w:r><w:t>{s}</w:t></w:r></w:p></w:tc></w:tr>");
    let tpl = docx(&format!("<w:tbl>{}{}</w:tbl>", row("{{#items}}{{name}}"), row("{{price}}{{/items}}")));
    assert_eq!(body(&render(&tpl, r#"{"items":[{"name":"a","price":1},{"name":"b","price":2}]}"#)), "a\n1\nb\n2");
}

#[test]
fn provides_loop_locals_and_parent_scope_access() {
    let tpl =
        docx(&p(&["{{#items}}{{@number}}/{{@length}} {{name}} of {{../title}}{{#@last}}.{{/@last}}{{^@last}}, {{/@last}}{{/items}}"]));
    assert_eq!(body(&render(&tpl, r#"{"title":"T","items":[{"name":"a"},{"name":"b"}]}"#)), "1/2 a of T, 2/2 b of T.");
}

#[test]
fn looks_up_outer_variables_from_inside_loops() {
    assert_eq!(body(&render(&docx(&p(&["{{#items}}{{currency}}{{.}} {{/items}}"])), r#"{"currency":"$","items":[1,2]}"#)), "$1 $2 ");
}

#[test]
fn nests_loops() {
    let tpl = docx(&p(&["{{#groups}}{{name}}:{{#items}}{{.}}{{/items}};{{/groups}}"]));
    assert_eq!(body(&render(&tpl, r#"{"groups":[{"name":"a","items":[1,2]},{"name":"b","items":[3]}]}"#)), "a:12;b:3;");
}

#[test]
fn uses_an_object_as_context() {
    assert_eq!(
        body(&render(&docx(&p(&["{{#client}}{{name}} ({{city}}){{/client}}"])), r#"{"client":{"name":"Ann","city":"Oslo"}}"#)),
        "Ann (Oslo)"
    );
}

#[test]
fn iterates_objects_with_each_and_key() {
    assert_eq!(body(&render(&docx(&p(&["{{#each prices}}{{@key}}={{.}};{{/each}}"])), r#"{"prices":{"a":1,"b":2}}"#)), "a=1;b=2;");
}

#[test]
fn supports_if_else_unless_inverted() {
    let tpl = docx(&p(&["{{#if paid}}paid{{else}}due{{/if}}|{{#unless paid}}!{{/unless}}|{{^items}}none{{/items}}"]));
    assert_eq!(body(&render(&tpl, r#"{"paid":true,"items":[1]}"#)), "paid||");
    assert_eq!(body(&render(&tpl, r#"{"paid":false,"items":[]}"#)), "due|!|none");
}

#[test]
fn supports_else_in_paragraph_sections() {
    let tpl = docx(&[p(&["{{#if vip}}"]), p(&["VIP"]), p(&["{{else}}"]), p(&["Regular"]), p(&["{{/if}}"])].concat());
    assert_eq!(body(&render(&tpl, r#"{"vip":true}"#)), "VIP");
    assert_eq!(body(&render(&tpl, r#"{"vip":false}"#)), "Regular");
}

#[test]
fn accepts_filters_in_section_expressions() {
    let tpl = docx(&p(&[r#"{{#each items | sort: "n", "desc"}}{{n}}{{/each}}|{{#if total | gt: 100}}big{{else}}small{{/if}}"#]));
    assert_eq!(body(&render(&tpl, r#"{"items":[{"n":1},{"n":3},{"n":2}],"total":150}"#)), "321|big");
}

#[test]
fn closes_sections_with_a_generic_close() {
    assert_eq!(body(&render(&docx(&p(&["{{#items}}{{.}}{{/}}"])), r#"{"items":[1,2]}"#)), "12");
}

// ------------------------------------------------------------------------------- filters

#[test]
fn applies_built_in_filters() {
    let tpl = docx(&p(&[
        r#"{{name | upper}}|{{missing | default: "—"}}|{{n | number: 2}}|{{d | date: "DD.MM.YYYY"}}|{{list | join: " / "}}|{{text | truncate: 5}}"#,
    ]));
    let out = render(&tpl, r#"{"name":"ann","n":1234567.891,"d":{"$date":"2024-01-31"},"list":["a","b"],"text":"abcdefgh"}"#);
    assert_eq!(body(&out), "ANN|—|1 234 567,89|31.01.2024|a / b|abcd…");
}

#[test]
fn accepts_typographic_quotes_inserted_by_word_autocorrect() {
    assert_eq!(body(&render(&docx(&p(&["{{x | default: “none”}} {{x | default: «нет»}}"])), "{}")), "none нет");
}

#[test]
fn supports_host_filters_with_arguments() {
    let mut host = FnHost::new()
        .filter("money", |v, args| Ok(Value::string(format!("{} {}", coffee_core::num::to_fixed(v.js_number(), 2), args[0].js_string()))));
    let out = render_host(&docx(&p(&[r#"{{price | money: "RUB"}}"#])), r#"{"price":5}"#, opts(r#"{"hostFilters":["money"]}"#), &mut host)
        .unwrap();
    assert_eq!(body(&out), "5.00 RUB");
}

#[test]
fn host_filters_override_builtins_and_may_return_any_value() {
    let mut host = FnHost::new().filter("upper", |_, _| Ok(Value::str("host"))).filter("pick", |v, _| {
        let Value::Array(a) = v else { return Err("not an array".into()) };
        Ok(Value::array(a.iter().take(1).cloned().collect()))
    });
    let tpl = docx(&p(&["{{x | upper}}|{{#each list | pick}}{{.}}{{/each}}"]));
    let out = render_host(&tpl, r#"{"x":"a","list":[1,2,3]}"#, opts(r#"{"hostFilters":["upper","pick"]}"#), &mut host).unwrap();
    assert_eq!(body(&out), "host|1");
}

#[test]
fn passes_variables_as_filter_arguments() {
    assert_eq!(body(&render(&docx(&p(&["{{a | default: b}}"])), r#"{"b":"fallback"}"#)), "fallback");
}

#[test]
fn rejects_unknown_filters_at_compile_time() {
    let e = compile(&docx(&p(&["{{x | nope}}"])), Options::default()).unwrap_err();
    assert_eq!(e.0[0].code, ErrorCode::UnknownFilter);
    assert!(e.0[0].message.contains(r#"Unknown filter "nope""#));
}

#[test]
fn wraps_host_filter_failures() {
    let mut host = FnHost::new().filter("boom", |_, _| Err("bad".into()));
    let e = render_host(&docx(&p(&["{{x | boom}}"])), "{}", opts(r#"{"hostFilters":["boom"]}"#), &mut host).unwrap_err();
    assert_eq!(e.code, ErrorCode::FilterFailed);
    assert!(e.message.contains("Filter failed: bad"));
    assert_eq!(e.details["hostMessage"], "bad");
}

#[test]
fn reports_host_error_without_callbacks() {
    let tpl = compile(&docx(&p(&["{{x | money}}"])), opts(r#"{"hostFilters":["money"]}"#)).unwrap();
    assert_eq!(tpl.render(&data("{}"), &mut coffee_core::NoHost).unwrap_err().code, ErrorCode::HostError);
}

// ------------------------------------------------------------------------------- errors

fn code(tpl: &[u8]) -> ErrorCode {
    compile(tpl, Options::default()).unwrap_err().0[0].code
}

#[test]
fn reports_template_mistakes_with_codes() {
    assert_eq!(code(&docx(&p(&["{{name"]))), ErrorCode::UnclosedTag);
    assert_eq!(code(&docx(&p(&["{{#items}}x"]))), ErrorCode::UnclosedSection);
    assert_eq!(code(&docx(&p(&["x{{/items}}"]))), ErrorCode::UnmatchedClose);
    assert_eq!(code(&docx(&p(&["{{#a}}{{/b}}"]))), ErrorCode::UnmatchedClose);
    assert_eq!(code(&docx(&p(&["{{a b}}"]))), ErrorCode::InvalidExpression);
    assert_eq!(code(&docx(&p(&["{{}}"]))), ErrorCode::InvalidExpression);
}

#[test]
fn includes_the_part_and_tag() {
    let e = &compile(&docx(&p(&["{{#items}}"])), Options::default()).unwrap_err().0[0];
    assert_eq!(e.tag.as_deref(), Some("{{#items}}"));
    assert_eq!(e.part.as_deref(), Some("word/document.xml"));
    assert!(e.to_string().contains("tag {{#items}} in word/document.xml"));
}

#[test]
fn collects_all_template_errors() {
    let tpl = docx(&[p(&["{{name"]), p(&["{{a b}}"]), p(&["{{x | nope}}"])].concat());
    let codes = compile(&tpl, Options::default()).unwrap_err().codes();
    assert_eq!(codes, vec![ErrorCode::UnclosedTag, ErrorCode::InvalidExpression, ErrorCode::UnknownFilter]);
}

#[test]
fn does_not_let_a_tag_span_paragraphs() {
    assert_eq!(code(&docx(&[p(&["{{na"]), p(&["me}}"])].concat())), ErrorCode::UnclosedTag);
}

#[test]
fn rejects_sections_crossing_structure_boundaries() {
    let tpl = docx(&format!("{}<w:tbl><w:tr><w:tc>{}</w:tc></w:tr></w:tbl>", p(&["{{#a}} text"]), p(&["{{/a}}"])));
    assert_eq!(code(&tpl), ErrorCode::InvalidSection);
}

#[test]
fn enforces_loop_limits() {
    let tpl = compile(&docx(&p(&["{{#items}}{{.}}{{/items}}"])), opts(r#"{"limits":{"maxLoopIterations":2}}"#)).unwrap();
    let e = tpl.render(&data(r#"{"items":[1,2,3]}"#), &mut coffee_core::NoHost).unwrap_err();
    assert_eq!(e.code, ErrorCode::LimitExceeded);
    assert_eq!(e.details["limit"], "maxLoopIterations");
}

// ------------------------------------------------------------------------------- fixture: invoice.docx

fn invoice_data(logo: &str) -> String {
    format!(
        r#"{{"company":"ACME","number":42,"client":{{"name":"Иван"}},"items":[{{"name":"Coffee","qty":2,"price":10.5}},{{"name":"Tea","qty":1,"price":4}}],"paid":true,"notes":"line 1\nline 2","logo":{logo},"total":25}}"#
    )
}

fn logo() -> String {
    format!(r#"{{"data":{{"$base64":"{PNG_2X1}"}},"width":120}}"#)
}

#[test]
fn invoice_renders_body_and_header() {
    let out = render(&fixture("invoice.docx"), &invoice_data(&logo()));
    let text = body(&out);
    assert!(text.contains("Invoice 42"), "{text}");
    assert!(text.contains("Client: Иван"));
    assert!(text.contains("- Coffee: 10,50\n- Tea: 4,00"));
    assert!(text.contains("Status: paid"));
    assert!(text.contains("Escaped: {{literal}}"));
    assert!(text.contains("Total: 25,00\u{a0}€"), "{text}");
    assert!(!text.replace("{{literal}}", "").contains("{{"));
    let header = part_names(&out).into_iter().find(|n| n.starts_with("word/header") && n.ends_with(".xml")).unwrap();
    assert!(read_part(&out, &header).contains("ACME — confidential"));
}

#[test]
fn invoice_embeds_images_with_relationships_and_content_types() {
    let out = render(&fixture("invoice.docx"), &invoice_data(&logo()));
    assert!(part_names(&out).iter().any(|n| n == "word/media/image_ct1.png"));
    assert!(read_part(&out, "[Content_Types].xml").contains(r#"Extension="png""#));
    let rels = read_part(&out, "word/_rels/document.xml.rels");
    let re = regex_lite::Regex::new(r#"Id="(rId\d+)"[^>]*Target="media/image_ct1\.png""#).unwrap();
    let r_id = re.captures(&rels).map(|c| c[1].to_string()).expect("image relationship");
    let doc = read_part(&out, "word/document.xml");
    assert!(doc.contains(&format!(r#"r:embed="{r_id}""#)));
    // 120px wide, aspect ratio 2:1 from the PNG header → 60px high
    assert!(doc.contains(&format!(r#"cx="{}" cy="{}""#, 120 * 9525, 60 * 9525)));
}

#[test]
fn invoice_removes_the_image_tag_when_no_image_is_given() {
    let out = render(&fixture("invoice.docx"), &invoice_data("null"));
    assert!(!read_part(&out, "word/document.xml").contains("<w:drawing>"));
}

#[test]
fn invoice_renders_a_compiled_template_many_times_independently() {
    let tpl = compile(&fixture("invoice.docx"), Options::default()).unwrap();
    let r = |json: String| tpl.render(&data(&json), &mut coffee_core::NoHost).unwrap();
    let a = body(&r(invoice_data(&logo()).replace("\"number\":42", "\"number\":1")));
    let b = body(&r(invoice_data(&logo()).replace("\"number\":42", "\"number\":2")));
    assert!(a.contains("Invoice 1"));
    assert!(b.contains("Invoice 2"));
    assert_eq!(part_names(&r(invoice_data("null"))).iter().filter(|n| n.contains("media")).count(), 0);
}

#[test]
fn invoice_output_is_deterministic() {
    let tpl = compile(&fixture("invoice.docx"), Options::default()).unwrap();
    let a = tpl.render(&data(&invoice_data(&logo())), &mut coffee_core::NoHost).unwrap();
    let b = tpl.render(&data(&invoice_data(&logo())), &mut coffee_core::NoHost).unwrap();
    assert_eq!(a, b);
    assert_eq!(part_names(&a)[0], "[Content_Types].xml");
}

#[test]
fn invoice_inspects_variables_and_the_expected_data_shape() {
    let info = compile(&fixture("invoice.docx"), Options::default()).unwrap().inspect();
    assert_eq!(info.format, coffee_core::DocumentFormat::Docx);
    for v in ["company", "number", "client.name", "items[].name", "items[].price", "items[].qty", "paid", "notes", "logo", "total"] {
        assert!(info.variables.iter().any(|x| x == v), "missing {v} in {:?}", info.variables);
    }
    let s = serde_json::Value::Object(info.schema);
    assert_eq!(s["company"], "");
    assert_eq!(s["client"]["name"], "");
    assert_eq!(s["items"][0]["name"], "");
    assert_eq!(s["items"][0]["price"], "");
    assert_eq!(s["items"][0]["qty"], "");
    assert_eq!(s["paid"], true);
    assert!(s["logo"].is_null());
}
