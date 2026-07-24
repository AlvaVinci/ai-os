#[cfg(target_os = "linux")]
use std::env;
#[cfg(target_os = "linux")]
use std::ffi::OsString;
#[cfg(target_os = "linux")]
use std::fs::{self, File, OpenOptions};
#[cfg(target_os = "linux")]
use std::io::Write;
#[cfg(target_os = "linux")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "linux")]
use std::process::Command;
use std::process::ExitCode;

const FAILURE_EXIT_CODE: u8 = 125;
#[cfg(target_os = "linux")]
const MAX_LAUNCH_ARGUMENTS: usize = 128;
#[cfg(target_os = "linux")]
const MAX_PATH_BYTES: usize = 4_096;

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(()) => ExitCode::from(FAILURE_EXIT_CODE),
    }
}

#[cfg(target_os = "linux")]
fn run() -> Result<(), ()> {
    use std::os::unix::process::CommandExt;

    let mut arguments = env::args_os();
    let _program = arguments.next().ok_or(())?;
    let cgroup_procs = PathBuf::from(arguments.next().ok_or(())?);
    let expected_device = parse_u64(arguments.next().ok_or(())?)?;
    let expected_inode = parse_u64(arguments.next().ok_or(())?)?;
    let executable = PathBuf::from(arguments.next().ok_or(())?);
    let child_arguments: Vec<OsString> = arguments.collect();
    if child_arguments.len() > MAX_LAUNCH_ARGUMENTS {
        return Err(());
    }
    let mut cgroup_procs = open_cgroup_procs(&cgroup_procs, expected_device, expected_inode)?;
    validate_executable(&executable)?;

    cgroup_procs
        .write_all(std::process::id().to_string().as_bytes())
        .map_err(|_| ())?;

    let mut command = Command::new(executable);
    command.args(child_arguments).env_clear();
    let _error = command.exec();
    Err(())
}

#[cfg(not(target_os = "linux"))]
fn run() -> Result<(), ()> {
    Err(())
}

#[cfg(target_os = "linux")]
fn open_cgroup_procs(path: &Path, expected_device: u64, expected_inode: u64) -> Result<File, ()> {
    use std::os::unix::fs::MetadataExt;

    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES
        || path.file_name() != Some("cgroup.procs".as_ref())
    {
        return Err(());
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| ())?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(());
    }
    let file = OpenOptions::new().write(true).open(path).map_err(|_| ())?;
    let parent = path.parent().ok_or(())?;
    let parent_metadata = fs::metadata(parent).map_err(|_| ())?;
    if !parent_metadata.is_dir()
        || parent_metadata.dev() != expected_device
        || parent_metadata.ino() != expected_inode
    {
        return Err(());
    }
    Ok(file)
}

#[cfg(target_os = "linux")]
fn validate_executable(path: &Path) -> Result<(), ()> {
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES {
        return Err(());
    }
    let metadata = fs::metadata(path).map_err(|_| ())?;
    if !metadata.is_file() {
        return Err(());
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn parse_u64(value: OsString) -> Result<u64, ()> {
    value.into_string().map_err(|_| ())?.parse().map_err(|_| ())
}
