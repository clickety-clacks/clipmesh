use std::{env, fs, process::ExitCode};

use clipmesh_hub_edge::EdgeConfig;

fn main() -> ExitCode {
    let mut arguments = env::args_os().skip(1);
    let Some(path) = arguments.next() else {
        eprintln!("config_parse_failed");
        return ExitCode::from(64);
    };
    if arguments.next().is_some() {
        eprintln!("config_parse_failed");
        return ExitCode::from(64);
    }
    let Ok(contents) = fs::read_to_string(path) else {
        eprintln!("config_parse_failed");
        return ExitCode::from(1);
    };
    match EdgeConfig::parse_toml(&contents) {
        Ok(_) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{}", error.code());
            ExitCode::from(1)
        }
    }
}
