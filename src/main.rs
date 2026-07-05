use clap::Parser;
use serde::Deserialize;
use server::start_server;
use std::error::Error;
use std::fs::File;
use std::io::BufReader;
use std::time::{SystemTime, UNIX_EPOCH};

mod server;

#[derive(Parser, Debug)]
#[command(version, about = "Transparent Docker Proxy", long_about = None)]

struct Args {
    /// The port to listen on
    #[arg(short, long, default_value_t = 8080)]
    port: u16,

    /// The target Docker host
    #[arg(short, long, default_value = "./proxy.yaml")]
    config_file: String,

    //cache directory
    #[arg(long, default_value = "./cache")]
    cache_dir: String,

    #[arg(long)]
    ttl: Option<u64>,

    #[arg(short, long)]
    log_level: Option<u16>,
}

#[derive(Debug, Deserialize)]
pub struct DockerHost {
    name: String,
    url: String,
    username: Option<String>,
    token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Config {
    hosts: Vec<DockerHost>,
    ttl: Option<u64>,
    log_level: Option<u16>,
}

fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();

    log("main", &format!("Port: {}", args.port));
    log("main", &format!("Config file: {}", args.config_file));
    log("main", &format!("Cache directory: {}", args.cache_dir));
    log(
        "main",
        &format!(
            "Requested log level: {}",
            args.log_level
                .map(|value| value.to_string())
                .unwrap_or_else(|| "default".to_string())
        ),
    );

    let file = File::open(&args.config_file)?;

    let config: Config =
        yaml_serde::from_reader(BufReader::new(file)).expect("Failed to parse config file");
    log(
        "main",
        &format!("Loaded {} configured host(s)", config.hosts.len()),
    );
    let ttl = args.ttl.or(config.ttl).unwrap_or(31_557_600);
    let log_level = args.log_level.or(config.log_level).unwrap_or(0);

    log("main", &format!("TTL: {ttl}"));
    log("main", &format!("Log level: {log_level}"));

    if log_level >= 1 {
        for host in &config.hosts {
            log(
                "main",
                &format!(
                    "Configured host '{}' at {} (username: {}, token: {})",
                    host.name,
                    host.url,
                    present_or_missing(host.username.as_deref()),
                    present_or_missing(host.token.as_deref())
                ),
            );
        }
    }

    start_server(args.port, config, args.cache_dir, ttl, log_level)
}

fn present_or_missing(value: Option<&str>) -> &'static str {
    if value.is_some() {
        "present"
    } else {
        "missing"
    }
}

fn log(scope: &str, message: &str) {
    eprintln!("{} [{scope}] {message}", timestamp());
}

fn timestamp() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let seconds = now.as_secs();
    let millis = now.subsec_millis();

    format!("{seconds}.{millis:03}")
}
