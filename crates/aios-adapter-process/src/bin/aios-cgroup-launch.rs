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
use std::process::ExitCode;
#[cfg(target_os = "linux")]
use std::process::{Command, Stdio};

const FAILURE_EXIT_CODE: u8 = 125;
#[cfg(target_os = "linux")]
const MAX_LAUNCH_ARGUMENTS: usize = 512;
#[cfg(target_os = "linux")]
const MAX_DESCRIPTOR_MOUNTS: usize = 128;
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
    let mut arguments = env::args_os();
    let _program = arguments.next().ok_or(())?;
    let first = arguments.next().ok_or(())?;
    if first == "--descriptor-broker" {
        run_descriptor_broker(arguments)
    } else {
        run_legacy(PathBuf::from(first), arguments)
    }
}

#[cfg(target_os = "linux")]
fn run_legacy(
    cgroup_procs: PathBuf,
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<(), ()> {
    use std::os::unix::process::CommandExt;

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

#[cfg(target_os = "linux")]
fn run_descriptor_broker(mut arguments: impl Iterator<Item = OsString>) -> Result<(), ()> {
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let cgroup_procs = PathBuf::from(arguments.next().ok_or(())?);
    let expected_device = parse_u64(arguments.next().ok_or(())?)?;
    let expected_inode = parse_u64(arguments.next().ok_or(())?)?;
    let executable = PathBuf::from(arguments.next().ok_or(())?);
    let mount_count = parse_usize(arguments.next().ok_or(())?)?;
    if mount_count > MAX_DESCRIPTOR_MOUNTS {
        return Err(());
    }

    let mut mounts = Vec::with_capacity(mount_count);
    for _ in 0..mount_count {
        let access = arguments.next().ok_or(())?;
        if access != "read" {
            return Err(());
        }
        let destination = PathBuf::from(arguments.next().ok_or(())?);
        validate_sandbox_path(&destination)?;
        mounts.push(destination);
    }
    let mut child_arguments: Vec<OsString> = arguments.collect();
    let descriptor_argument_count = mount_count
        .checked_mul(3)
        .and_then(|count| count.checked_add(2))
        .ok_or(())?;
    if child_arguments
        .len()
        .checked_add(descriptor_argument_count)
        .is_none_or(|count| count > MAX_LAUNCH_ARGUMENTS)
    {
        return Err(());
    }

    let mut cgroup_procs = open_cgroup_procs(&cgroup_procs, expected_device, expected_inode)?;
    validate_executable(&executable)?;
    let descriptors = receive_descriptors(mount_count.checked_add(1).ok_or(())?)?;
    if descriptors
        .iter()
        .any(|descriptor| descriptor.as_raw_fd() <= 2)
    {
        return Err(());
    }

    cgroup_procs
        .write_all(std::process::id().to_string().as_bytes())
        .map_err(|_| ())?;

    let delimiter = child_arguments
        .iter()
        .position(|argument| argument == "--")
        .ok_or(())?;
    let mut descriptor_arguments = Vec::with_capacity(descriptor_argument_count);
    descriptor_arguments.push(OsString::from("--seccomp"));
    descriptor_arguments.push(OsString::from(descriptors[0].as_raw_fd().to_string()));
    for (mount, descriptor) in mounts.iter().zip(descriptors.iter().skip(1)) {
        descriptor_arguments.push(OsString::from("--ro-bind-fd"));
        descriptor_arguments.push(OsString::from(descriptor.as_raw_fd().to_string()));
        descriptor_arguments.push(mount.as_os_str().to_owned());
    }
    child_arguments.splice(delimiter..delimiter, descriptor_arguments);

    for descriptor in &descriptors {
        rustix::io::fcntl_setfd(descriptor, rustix::io::FdFlags::empty()).map_err(|_| ())?;
    }

    let mut command = Command::new(executable);
    command
        .args(child_arguments)
        .env_clear()
        .stdin(Stdio::null());
    let _error = command.exec();
    Err(())
}

#[cfg(target_os = "linux")]
fn receive_descriptors(expected: usize) -> Result<Vec<std::os::fd::OwnedFd>, ()> {
    use std::io::IoSliceMut;
    use std::mem::MaybeUninit;

    use rustix::net::{RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags, recvmsg};

    let mut payload = [0_u8; 1];
    let mut input = [IoSliceMut::new(&mut payload)];
    let mut control_space = vec![MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(expected))];
    let mut control = RecvAncillaryBuffer::new(&mut control_space);
    let received = recvmsg(
        rustix::stdio::stdin(),
        &mut input,
        &mut control,
        RecvFlags::CMSG_CLOEXEC,
    )
    .map_err(|_| ())?;
    if received.bytes != payload.len()
        || received
            .flags
            .intersects(ReturnFlags::CTRUNC | ReturnFlags::TRUNC)
        || payload != [1]
    {
        return Err(());
    }

    let mut descriptors = Vec::with_capacity(expected);
    for message in control.drain() {
        if let RecvAncillaryMessage::ScmRights(rights) = message {
            descriptors.extend(rights);
        }
    }
    if descriptors.len() != expected {
        return Err(());
    }
    Ok(descriptors)
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

#[cfg(target_os = "linux")]
fn parse_usize(value: OsString) -> Result<usize, ()> {
    value.into_string().map_err(|_| ())?.parse().map_err(|_| ())
}

#[cfg(target_os = "linux")]
fn validate_sandbox_path(path: &Path) -> Result<(), ()> {
    use std::path::Component;

    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().len() > MAX_PATH_BYTES {
        return Err(());
    }
    let mut components = path.components();
    if components.next() != Some(Component::RootDir)
        || !components.all(|component| matches!(component, Component::Normal(_)))
    {
        return Err(());
    }
    Ok(())
}
