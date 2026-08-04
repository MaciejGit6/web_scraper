#![cfg(target_os = "linux")]

mod error;
mod cli;
mod sync;
mod input;
mod state;

//files with utilities:
use crate::cli::parse_arguments;
use crate::input::{DomainFile, OptionsFile};
use crate::state::SharedCoordinator;
use std::io;


pub fn run() -> io::Result<()> {
    let arguments = parse_arguments()?;

    let domains = DomainFile::open(&arguments.input_path)?;

    
    let options = arguments
        .options_path
        .as_deref()
        .map(OptionsFile::open)
        .transpose()?;

    
    let coordinator = SharedCoordinator::open_or_create(&arguments.state_path, &domains)?;

    println!(
        "Mapped domain input: {} ({} bytes)",
        domains.path().display(),
        domains.len()
    );
    println!("Shared state: {}", coordinator.path().display());

    if let Some(options) = &options {
        println!(
            "Mapped optional options file: {} ({} bytes; not interpreted yet)",
            options.path().display(),
            options.len()
        );
    }

    println!("Ready. No domains are processed by this base program.");


    Ok(())
}



fn main() {
    if let Err(error) = run() {
        eprintln!("Error: {error}");
        std::process::exit(1);
    }
}