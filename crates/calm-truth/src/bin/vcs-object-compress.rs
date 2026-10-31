//! Local operator tool. Never migrates, deploys, deletes history or VACUUMs.
use calm_truth::db::sqlite::SqliteDb;
use calm_truth::track_vcs::object_compression::{convert_page, preview_page, require_schema};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use std::str::FromStr;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    const USAGE: &str =
        "usage: vcs-object-compress <sqlite-url> [--apply] [--after <hash>] [--pages <count>]";
    let mut args = std::env::args().skip(1);
    let url = args.next().ok_or(USAGE)?;
    if url == "--help" {
        println!("{USAGE}");
        return Ok(());
    }
    let mut apply = false;
    let mut after = String::new();
    let mut pages = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apply" => apply = true,
            "--after" => after = args.next().ok_or("--after requires a cursor")?,
            "--pages" => {
                let count = args
                    .next()
                    .ok_or("--pages requires a count")?
                    .parse::<u64>()?;
                if count == 0 {
                    return Err("--pages must be positive".into());
                }
                pages = Some(count);
            }
            _ => return Err(format!("unknown option: {arg}").into()),
        }
    }
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(async {
            let reader = SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(
                    SqliteConnectOptions::from_str(&url)?
                        .read_only(true)
                        .create_if_missing(false),
                )
                .await?;
            require_schema(&reader).await?;
            let db = if apply {
                Some(SqliteDb::connect_existing(&url).await?)
            } else {
                None
            };
            let mut completed_pages = 0;
            loop {
                let page = match &db {
                    Some(db) => convert_page(db, &after).await?,
                    None => preview_page(&reader, &after).await?,
                };
                println!("{}", serde_json::to_string(&page)?);
                after = page.after;
                completed_pages += 1;
                if page.complete || pages.is_some_and(|limit| completed_pages >= limit) {
                    break;
                }
                tokio::task::yield_now().await;
            }
            if let Some(db) = db {
                db.close().await;
            }
            reader.close().await;
            Ok::<_, Box<dyn std::error::Error>>(())
        })
}
