//! Generate, parse, transform and re-serialise a JSON document with serde_json.
use serde_json::{json, Value};

fn main() {
    let n: usize = std::env::args().nth(1).and_then(|a| a.parse().ok()).unwrap_or(20_000);
    let mut seed = 7u64;
    let mut rnd = move || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        seed >> 33
    };
    let items: Vec<Value> = (0..n)
        .map(|i| {
            json!({
                "id": i,
                "name": format!("item-{}", rnd() % 100_000),
                "price": (rnd() % 100_000) as f64 / 100.0,
                "tags": (0..(rnd() % 5)).map(|t| format!("t{t}")).collect::<Vec<_>>(),
                "in_stock": rnd() % 2 == 0,
                "dims": {"w": rnd() % 500, "h": rnd() % 500},
            })
        })
        .collect();
    let text = serde_json::to_string(&Value::Array(items)).unwrap();
    let doc: Value = serde_json::from_str(&text).unwrap();
    let arr = doc.as_array().unwrap();
    let mut total = 0.0f64;
    let mut stocked = 0usize;
    let mut tags = 0usize;
    for it in arr {
        total += it["price"].as_f64().unwrap();
        if it["in_stock"].as_bool().unwrap() {
            stocked += 1;
        }
        tags += it["tags"].as_array().unwrap().len();
    }
    let again = serde_json::to_string_pretty(&doc).unwrap();
    let back: Value = serde_json::from_str(&again).unwrap();
    assert_eq!(back, doc);
    println!("{} bytes, {} items, {stocked} in stock, {tags} tags, total price {total:.2}, pretty {} bytes",
             text.len(), arr.len(), again.len());
}
