use std::env;
use std::path::PathBuf;
use std::process::ExitCode;

use aios_adapter_process::build_minimal_root_filesystem;

fn main() -> ExitCode {
    let mut arguments = env::args_os();
    let _program = arguments.next();
    let Some(busybox) = arguments.next() else {
        return usage();
    };
    let Some(output) = arguments.next() else {
        return usage();
    };
    if arguments.next().is_some() {
        return usage();
    }

    match build_minimal_root_filesystem(PathBuf::from(busybox), PathBuf::from(output)) {
        Ok(digest) => {
            println!("{digest}");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn usage() -> ExitCode {
    eprintln!("usage: aios-rootfs-build <absolute-busybox-path> <absolute-output-path>");
    ExitCode::from(2)
}
