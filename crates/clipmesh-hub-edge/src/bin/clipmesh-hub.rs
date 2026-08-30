use std::{env, fs, process::ExitCode};

use clipmesh_hub_edge::{EdgeConfig, HubEdge};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(code) => {
            eprintln!("{code}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<(), &'static str> {
    let mut arguments = env::args_os().skip(1);
    if arguments.next().as_deref() != Some("--config".as_ref()) {
        return Err("config_missing_required");
    }
    let path = arguments.next().ok_or("config_missing_required")?;
    if arguments.next().is_some() {
        return Err("config_unknown_field");
    }
    let text = fs::read_to_string(path).map_err(|_| "config_parse_failed")?;
    let config = EdgeConfig::parse_toml(&text).map_err(|error| error.code())?;
    let listener = HubEdge::bind(config).map_err(|error| error.0.code())?;
    listener.serve().map_err(|error| error.0.code())
}
