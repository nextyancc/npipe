use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::BufReader;

#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    pub database_url: String,
}

pub static GLOBAL_CONFIG: Lazy<Config> = Lazy::new(|| {
    let file = match File::open("config.json") {
        Ok(file) => file,
        Err(e) => {
            eprintln!("Failed to open config file: {}", e);
            std::process::exit(1);
        }
    };
    let reader = BufReader::new(file);
    match serde_json::from_reader(reader) {
        Ok(cfg) => cfg,
        Err(e) => {
            eprintln!("Failed to parse config file: {}", e);
            std::process::exit(1);
        }
    }
});
