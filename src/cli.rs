use crate::error::invalid_input;

use std::env;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};

#[derive(Debug)]
pub(crate) struct Arguments {
    pub(crate) input_path: PathBuf,
    pub(crate) options_path: Option<PathBuf>,
    pub(crate) state_path: PathBuf,
}

pub(crate) fn parse_arguments() -> io::Result<Arguments> {
    let mut arguments = env::args_os();
    let program = arguments
        .next()
        .unwrap_or_else(|| OsString::from("rust_mmap_sync"));

    let input_path = match arguments.next() {
        Some(value)
            if value.as_os_str() != OsStr::new("-h")
                && value.as_os_str() != OsStr::new("--help") =>
        {
            PathBuf::from(value)
        }
        _ => {
            print_usage(&program);
            return Err(invalid_input("missing required domain input file"));
        }
    };

    let mut options_path = None;
    let mut state_path = None;

    while let Some(argument) = arguments.next() {
        if argument.as_os_str() == OsStr::new("-o")
            || argument.as_os_str() == OsStr::new("--options")
        {
            let value = arguments
                .next()
                .ok_or_else(|| invalid_input("missing path after --options"))?;
            options_path = Some(PathBuf::from(value));
        } else if argument.as_os_str() == OsStr::new("--state") {
            let value = arguments
                .next()
                .ok_or_else(|| invalid_input("missing path after --state"))?;
            state_path = Some(PathBuf::from(value));
        } else if argument.as_os_str() == OsStr::new("-h")
            || argument.as_os_str() == OsStr::new("--help")
        {
            print_usage(&program);
            std::process::exit(0);
        } else {
            print_usage(&program);
            return Err(invalid_input(format!(
                "unknown argument: {}",
                argument.to_string_lossy()
            )));
        }
    }

    let state_path = state_path.unwrap_or_else(|| default_state_path(&input_path));

    Ok(Arguments {
        input_path,
        options_path,
        state_path,
    })
}

pub fn default_state_path(input_path: &Path) -> PathBuf {
    let mut value = input_path.as_os_str().to_os_string();
    value.push(".state");
    PathBuf::from(value)
}


pub fn print_usage(program: &OsString) {
    eprintln!(
        "Usage:\n  {} <domains-file> [--options <options-file>] [--state <state-file>]",
        Path::new(program.as_os_str()).display()
    );
}
