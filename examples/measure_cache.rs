//! Measures local persistence and thread ordering, not rendering or network latency.
use rustvn::{
    article::article_order,
    model::{Article, Group, Server, Sort},
    store::Store,
};
use std::{collections::HashSet, time::Instant};
fn main() -> anyhow::Result<()> {
    let temp = tempfile::tempdir()?;
    let mut store = Store::open(&temp.path().join("measure.db"))?;
    let server = Server::default();
    store.save_server(&server)?;
    let start = Instant::now();
    let groups: Vec<_> = (0..100_000)
        .map(|i| Group {
            name: format!("fj.fixture.{i:06}"),
            low: 1,
            high: 100_000,
            ..Default::default()
        })
        .collect();
    store.upsert_groups(&server.id, &groups)?;
    println!("100,000 groups: save {:?}", start.elapsed());
    let articles: Vec<_> = (1..=100_000)
        .map(|n| Article {
            number: n,
            message_id: format!("<{n}@fixture>"),
            subject: format!("記事 {n}"),
            timestamp: n as i64,
            references: if n > 1 {
                vec![format!("<{}@fixture>", n - 1)]
            } else {
                vec![]
            },
            ..Default::default()
        })
        .collect();
    let start = Instant::now();
    store.save_overview(
        &server.id,
        "fj.fixture.000000",
        (1, 100_000),
        Some((1, 100_000)),
        &articles,
    )?;
    println!("100,000 article summaries: save {:?}", start.elapsed());
    let start = Instant::now();
    let groups = store.groups(&server.id)?;
    println!("{} groups: load {:?}", groups.len(), start.elapsed());
    let start = Instant::now();
    let articles = store.articles(&server.id, "fj.fixture.000000", "")?;
    println!("{} summaries: load {:?}", articles.len(), start.elapsed());
    let start = Instant::now();
    let order = article_order(&articles, true, Sort::Date, &HashSet::new());
    println!(
        "{} articles: deep thread ordering {:?}",
        order.len(),
        start.elapsed()
    );
    Ok(())
}
