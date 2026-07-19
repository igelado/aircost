use std::env;

use aircost_rs::db::{database_url_from_arg, DEFAULT_DATABASE_PATH};
use aircost_rs::server::{run_server, ServerConfig};
use anyhow::{bail, Context, Result};

#[tokio::main]
async fn main() -> Result<()> {
    let config = parse_args(env::args().skip(1))?;
    run_server(config).await
}

fn parse_args(args: impl IntoIterator<Item = String>) -> Result<ServerConfig> {
    let mut host = "127.0.0.1".to_string();
    let mut port = 8000_u16;
    let mut database = None;
    let mut args = args.into_iter();

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--host" => {
                host = args
                    .next()
                    .context("--host requires a value")?
                    .trim()
                    .to_string();
                if host.is_empty() {
                    bail!("--host cannot be empty");
                }
            }
            "--port" => {
                let value = args.next().context("--port requires a value")?;
                port = value
                    .parse::<u16>()
                    .with_context(|| format!("invalid --port value: {value}"))?;
            }
            "--database" => {
                database = Some(args.next().context("--database requires a value")?);
            }
            "--database-url" => {
                database = Some(args.next().context("--database-url requires a value")?);
            }
            "--help" | "-h" => {
                println!(
                    "Usage: aircost-web [--host 127.0.0.1] [--port 8000] [--database {DEFAULT_DATABASE_PATH}] [--database-url sqlite://data/aircost.sqlite3|postgres://...]"
                );
                std::process::exit(0);
            }
            _ => bail!("unknown argument: {arg}"),
        }
    }

    Ok(ServerConfig {
        host,
        port,
        database_url: database_url_from_arg(database),
    })
}
