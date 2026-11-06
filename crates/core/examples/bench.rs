//! Rendering benchmark: `cargo run -p coffee-core --release --example bench -- [rows]`.

use coffee_core::{value::parse_data, NoHost, Options, Template};
use std::time::Instant;

fn main() {
    let rows: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(10_000);
    let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../conformance/fixtures/");
    let items: Vec<String> = (0..rows).map(|i| format!(r#"{{"name":"Item {i}","qty":{},"price":{}.5}}"#, i % 7 + 1, i % 100)).collect();
    let items = items.join(",");
    for (file, data) in [
        ("report.xlsx", format!(r#"{{"title":"Bench","items":[{items}],"author":"me"}}"#)),
        ("invoice.docx", format!(r#"{{"number":1,"client":{{"name":"A"}},"items":[{items}],"paid":true}}"#)),
    ] {
        let bytes = std::fs::read(format!("{dir}{file}")).unwrap();
        let data = parse_data(&data, &[]).unwrap();
        let t = Instant::now();
        let tpl = Template::compile(&bytes, Options::default()).unwrap();
        let compile = t.elapsed();
        let t = Instant::now();
        let n = 5;
        let mut size = 0;
        for _ in 0..n {
            size = tpl.render(&data, &mut NoHost).unwrap().len();
        }
        let per = t.elapsed() / n;
        println!(
            "{file:13} rows={rows:6} compile={compile:?} render={per:?} ({:.1} µs/row) out={} KiB",
            per.as_secs_f64() * 1e6 / rows as f64,
            size / 1024
        );
    }
}
