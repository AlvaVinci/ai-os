use std::path::Path;
#[cfg(target_os = "linux")]
use std::path::PathBuf;

use aios_core::{FileAccess, FileCapability};

use super::{ProcessAdapterError, canonical_directory, validate_sandbox_absolute_path};

const MAX_FILESYSTEM_CAPABILITY_MOUNTS: usize = 128;

pub(crate) struct FilesystemCapabilityMount {
    capability: FileCapability,
    #[cfg(target_os = "linux")]
    descriptor: std::os::fd::OwnedFd,
}

#[cfg(target_os = "linux")]
pub(crate) struct DescriptorSender(std::os::unix::net::UnixStream);
#[cfg(not(target_os = "linux"))]
pub(crate) struct DescriptorSender;

impl FilesystemCapabilityMount {
    pub(crate) fn capability(&self) -> &FileCapability {
        &self.capability
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn descriptor(&self) -> &std::os::fd::OwnedFd {
        &self.descriptor
    }
}

pub(crate) fn open_filesystem_capabilities(
    source_root: &Path,
    capabilities: Vec<FileCapability>,
) -> Result<Vec<FilesystemCapabilityMount>, ProcessAdapterError> {
    if capabilities.is_empty() {
        return Ok(Vec::new());
    }
    if !cfg!(target_os = "linux") {
        return Err(ProcessAdapterError::UnsupportedPlatform);
    }

    let source_root = canonical_directory(source_root)?;
    validate_capabilities(&capabilities)?;
    open_capabilities(&source_root, capabilities)
}

fn validate_capabilities(capabilities: &[FileCapability]) -> Result<(), ProcessAdapterError> {
    if capabilities.len() > MAX_FILESYSTEM_CAPABILITY_MOUNTS {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    let mut previous: Option<(&str, FileAccess)> = None;
    let mut ordered: Vec<_> = capabilities.iter().collect();
    ordered.sort_by(|left, right| {
        left.path
            .cmp(&right.path)
            .then_with(|| left.access.cmp(&right.access))
    });

    for capability in ordered {
        validate_sandbox_absolute_path(Path::new(&capability.path))?;
        if capability.path == "/" {
            return Err(ProcessAdapterError::InvalidSandbox);
        }
        if capability.access != FileAccess::Read {
            return Err(ProcessAdapterError::UnsupportedFilesystemCapability);
        }
        let current = (capability.path.as_str(), capability.access);
        if previous == Some(current) {
            return Err(ProcessAdapterError::InvalidSandbox);
        }
        previous = Some(current);
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn open_capabilities(
    source_root: &Path,
    mut capabilities: Vec<FileCapability>,
) -> Result<Vec<FilesystemCapabilityMount>, ProcessAdapterError> {
    use rustix::fs::{Mode, OFlags, ResolveFlags, openat2};

    let root = std::fs::File::open(source_root).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    capabilities.sort_by(|left, right| {
        path_depth(&left.path)
            .cmp(&path_depth(&right.path))
            .then_with(|| left.path.cmp(&right.path))
    });

    capabilities
        .into_iter()
        .map(|capability| {
            let relative = Path::new(&capability.path)
                .strip_prefix("/")
                .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
            let descriptor = openat2(
                &root,
                relative,
                OFlags::PATH | OFlags::CLOEXEC,
                Mode::empty(),
                ResolveFlags::BENEATH
                    | ResolveFlags::NO_MAGICLINKS
                    | ResolveFlags::NO_SYMLINKS
                    | ResolveFlags::NO_XDEV,
            )
            .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
            Ok(FilesystemCapabilityMount {
                capability,
                descriptor,
            })
        })
        .collect()
}

#[cfg(not(target_os = "linux"))]
fn open_capabilities(
    _source_root: &Path,
    _capabilities: Vec<FileCapability>,
) -> Result<Vec<FilesystemCapabilityMount>, ProcessAdapterError> {
    Err(ProcessAdapterError::UnsupportedPlatform)
}

#[cfg(target_os = "linux")]
fn path_depth(path: &str) -> usize {
    PathBuf::from(path).components().count()
}

#[cfg(target_os = "linux")]
pub(crate) fn descriptor_channel()
-> Result<(DescriptorSender, std::process::Stdio), ProcessAdapterError> {
    use std::os::fd::OwnedFd;

    let (sender, receiver) = std::os::unix::net::UnixStream::pair()
        .map_err(|_| ProcessAdapterError::DescriptorTransferFailed)?;
    let receiver: OwnedFd = receiver.into();
    Ok((
        DescriptorSender(sender),
        std::process::Stdio::from(receiver),
    ))
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn descriptor_channel()
-> Result<(DescriptorSender, std::process::Stdio), ProcessAdapterError> {
    Err(ProcessAdapterError::UnsupportedPlatform)
}

#[cfg(target_os = "linux")]
impl DescriptorSender {
    pub(crate) fn send(
        &self,
        seccomp: &super::seccomp::SeccompFilterFile,
        mounts: &[FilesystemCapabilityMount],
    ) -> Result<(), ProcessAdapterError> {
        use std::io::IoSlice;
        use std::mem::MaybeUninit;
        use std::os::fd::{AsFd, BorrowedFd};

        use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};

        let mut descriptors: Vec<BorrowedFd<'_>> =
            Vec::with_capacity(mounts.len().saturating_add(1));
        descriptors.push(seccomp.as_fd());
        descriptors.extend(mounts.iter().map(|mount| mount.descriptor().as_fd()));

        let mut control_space =
            vec![MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(descriptors.len()))];
        let mut control = SendAncillaryBuffer::new(&mut control_space);
        if !control.push(SendAncillaryMessage::ScmRights(&descriptors)) {
            return Err(ProcessAdapterError::DescriptorTransferFailed);
        }
        let payload = [1_u8];
        let sent = sendmsg(
            &self.0,
            &[IoSlice::new(&payload)],
            &mut control,
            SendFlags::NOSIGNAL,
        )
        .map_err(|_| ProcessAdapterError::DescriptorTransferFailed)?;
        if sent != payload.len() {
            return Err(ProcessAdapterError::DescriptorTransferFailed);
        }
        Ok(())
    }
}

#[cfg(not(target_os = "linux"))]
impl DescriptorSender {
    pub(crate) fn send(
        &self,
        _seccomp: &super::seccomp::SeccompFilterFile,
        _mounts: &[FilesystemCapabilityMount],
    ) -> Result<(), ProcessAdapterError> {
        Err(ProcessAdapterError::UnsupportedPlatform)
    }
}

#[cfg(test)]
mod tests {
    use aios_core::{FileAccess, FileCapability};

    use super::validate_capabilities;
    use crate::ProcessAdapterError;

    #[test]
    fn accepts_normalized_read_capabilities() {
        let capabilities = vec![
            FileCapability {
                path: "/workspace/project".to_owned(),
                access: FileAccess::Read,
            },
            FileCapability {
                path: "/opt/reference.txt".to_owned(),
                access: FileAccess::Read,
            },
        ];

        assert_eq!(validate_capabilities(&capabilities), Ok(()));
    }

    #[test]
    fn rejects_write_and_duplicate_capabilities() {
        let write = vec![FileCapability {
            path: "/workspace/output".to_owned(),
            access: FileAccess::Write,
        }];
        assert_eq!(
            validate_capabilities(&write),
            Err(ProcessAdapterError::UnsupportedFilesystemCapability)
        );

        let duplicate = vec![
            FileCapability {
                path: "/workspace/project".to_owned(),
                access: FileAccess::Read,
            },
            FileCapability {
                path: "/workspace/project".to_owned(),
                access: FileAccess::Read,
            },
        ];
        assert_eq!(
            validate_capabilities(&duplicate),
            Err(ProcessAdapterError::InvalidSandbox)
        );
    }
}
