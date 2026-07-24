use std::fmt::{self, Display, Formatter};
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use super::{DirectoryIdentity, ProcessAdapterError};

pub const MAX_ROOT_FILESYSTEM_ENTRIES: usize = 4_096;
pub const MAX_ROOT_FILESYSTEM_FILE_BYTES: u64 = 256 * 1_024 * 1_024;
pub const MAX_ROOT_FILESYSTEM_TOTAL_BYTES: u64 = 512 * 1_024 * 1_024;

const DIGEST_BYTES: usize = 32;
const DIGEST_HEX_BYTES: usize = DIGEST_BYTES * 2;
const DIGEST_DOMAIN: &[u8] = b"AIOS_ROOTFS_TREE_SHA256_V1\0";

/// Builds a sealed minimal rootfs containing one BusyBox executable and required mount points.
///
/// `output` must be an absolute path that does not exist. A partially built directory is retained
/// on failure and is never reused or removed automatically.
#[cfg(unix)]
pub fn build_minimal_root_filesystem(
    busybox: impl AsRef<Path>,
    output: impl AsRef<Path>,
) -> Result<RootFilesystemDigest, ProcessAdapterError> {
    use std::fs::{DirBuilder, File, OpenOptions};
    use std::io;
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};

    let busybox = busybox.as_ref();
    let output = output.as_ref();
    if !busybox.is_absolute() || output == Path::new("/") {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    super::validate_sandbox_absolute_path(output)?;
    let source_link =
        fs::symlink_metadata(busybox).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if source_link.file_type().is_symlink() || !source_link.is_file() {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    let source = fs::canonicalize(busybox).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    let source_metadata = fs::metadata(&source).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if !source_metadata.is_file()
        || source_metadata.mode() & 0o111 == 0
        || source_metadata.len() > MAX_ROOT_FILESYSTEM_FILE_BYTES
    {
        return Err(ProcessAdapterError::InvalidSandbox);
    }

    let mut directory_builder = DirBuilder::new();
    directory_builder.mode(0o700);
    directory_builder
        .create(output)
        .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    for relative in ["bin", "proc", "dev", "tmp", "workspace"] {
        directory_builder
            .create(output.join(relative))
            .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    }

    let mut source_file = File::open(&source).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    let opened_source = source_file
        .metadata()
        .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if opened_source.dev() != source_metadata.dev()
        || opened_source.ino() != source_metadata.ino()
        || opened_source.len() != source_metadata.len()
    {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    let mut destination_options = OpenOptions::new();
    destination_options.write(true).create_new(true).mode(0o500);
    let destination = output.join("bin/busybox");
    let mut destination_file = destination_options
        .open(&destination)
        .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    let copied = io::copy(&mut source_file, &mut destination_file)
        .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if copied != source_metadata.len() {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    drop(destination_file);

    fs::set_permissions(&destination, fs::Permissions::from_mode(0o555))
        .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    for relative in ["bin", "proc", "dev", "tmp", "workspace"] {
        fs::set_permissions(output.join(relative), fs::Permissions::from_mode(0o555))
            .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    }
    fs::set_permissions(output, fs::Permissions::from_mode(0o555))
        .map_err(|_| ProcessAdapterError::InvalidSandbox)?;

    let digest = RootFilesystemDigest::measure(output)?;
    VerifiedRootFilesystem::open(output, digest)?;
    Ok(digest)
}

#[cfg(not(unix))]
pub fn build_minimal_root_filesystem(
    _busybox: impl AsRef<Path>,
    _output: impl AsRef<Path>,
) -> Result<RootFilesystemDigest, ProcessAdapterError> {
    Err(ProcessAdapterError::UnsupportedPlatform)
}

/// SHA-256 identity of one canonical read-only root filesystem tree.
#[derive(Clone, Copy, Eq, PartialEq)]
pub struct RootFilesystemDigest([u8; DIGEST_BYTES]);

impl RootFilesystemDigest {
    #[must_use]
    pub const fn from_bytes(bytes: [u8; DIGEST_BYTES]) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub const fn as_bytes(&self) -> &[u8; DIGEST_BYTES] {
        &self.0
    }

    /// Measures a sealed tree. The returned value must be pinned outside that tree before it is
    /// later used as a trust anchor for [`VerifiedRootFilesystem::open`].
    pub fn measure(directory: impl AsRef<Path>) -> Result<Self, ProcessAdapterError> {
        let directory = canonical_read_only_root(directory.as_ref())?;
        measure_tree(&directory)
    }
}

impl Display for RootFilesystemDigest {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(formatter, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl FromStr for RootFilesystemDigest {
    type Err = ProcessAdapterError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        if value.len() != DIGEST_HEX_BYTES {
            return Err(ProcessAdapterError::InvalidSandbox);
        }
        let mut bytes = [0_u8; DIGEST_BYTES];
        for (index, pair) in value.as_bytes().chunks_exact(2).enumerate() {
            let high = decode_hex(pair[0])?;
            let low = decode_hex(pair[1])?;
            bytes[index] = high * 16 + low;
        }
        Ok(Self(bytes))
    }
}

/// Root filesystem tree verified against a digest pinned by trusted configuration.
///
/// The tree must contain only real read-only directories and regular files. This value does not
/// eliminate the host-side check-to-use race; callers should move to an fs-verity or equivalent
/// immutable backing store when one is available.
pub struct VerifiedRootFilesystem {
    directory: PathBuf,
    digest: RootFilesystemDigest,
    identity: DirectoryIdentity,
}

impl VerifiedRootFilesystem {
    pub fn open(
        directory: impl AsRef<Path>,
        expected_digest: RootFilesystemDigest,
    ) -> Result<Self, ProcessAdapterError> {
        let directory = canonical_read_only_root(directory.as_ref())?;
        super::validate_sandbox_mount_points(&directory)?;
        let identity = DirectoryIdentity::read(&directory)?;
        validate_tree(&directory, identity, expected_digest)?;
        Ok(Self {
            directory,
            digest: expected_digest,
            identity,
        })
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub fn digest(&self) -> RootFilesystemDigest {
        self.digest
    }

    pub(super) fn state(&self) -> VerifiedRootFilesystemState {
        VerifiedRootFilesystemState {
            digest: self.digest,
            identity: self.identity,
        }
    }
}

#[derive(Clone, Copy)]
pub(super) struct VerifiedRootFilesystemState {
    digest: RootFilesystemDigest,
    identity: DirectoryIdentity,
}

pub(super) fn validate_verified_root_filesystem(
    directory: &Path,
    state: VerifiedRootFilesystemState,
) -> Result<(), ProcessAdapterError> {
    validate_tree(directory, state.identity, state.digest)
}

fn decode_hex(byte: u8) -> Result<u8, ProcessAdapterError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        _ => Err(ProcessAdapterError::InvalidSandbox),
    }
}

fn validate_tree(
    directory: &Path,
    identity: DirectoryIdentity,
    expected_digest: RootFilesystemDigest,
) -> Result<(), ProcessAdapterError> {
    validate_read_only_directory(directory)?;
    if DirectoryIdentity::read(directory)? != identity
        || measure_tree(directory)? != expected_digest
    {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    Ok(())
}

#[cfg(unix)]
fn canonical_read_only_root(path: &Path) -> Result<PathBuf, ProcessAdapterError> {
    if !path.is_absolute()
        || path.as_os_str().as_encoded_bytes().len() > super::MAX_SANDBOX_PATH_BYTES
    {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    let metadata = fs::symlink_metadata(path).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    validate_read_only_directory(path)?;
    let canonical = fs::canonicalize(path).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if canonical == Path::new("/") {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    Ok(canonical)
}

#[cfg(not(unix))]
fn canonical_read_only_root(_path: &Path) -> Result<PathBuf, ProcessAdapterError> {
    Err(ProcessAdapterError::UnsupportedPlatform)
}

#[cfg(unix)]
fn validate_read_only_directory(path: &Path) -> Result<(), ProcessAdapterError> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = fs::symlink_metadata(path).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || metadata.permissions().mode() & 0o222 != 0
    {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_read_only_directory(_path: &Path) -> Result<(), ProcessAdapterError> {
    Err(ProcessAdapterError::UnsupportedPlatform)
}

#[cfg(unix)]
fn measure_tree(root: &Path) -> Result<RootFilesystemDigest, ProcessAdapterError> {
    use std::fs::File;
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    use sha2::{Digest, Sha256};

    let mut relative_paths = collect_relative_paths(root)?;
    relative_paths.sort_by(|left, right| {
        left.as_os_str()
            .as_encoded_bytes()
            .cmp(right.as_os_str().as_encoded_bytes())
    });

    let mut hasher = Sha256::new();
    hasher.update(DIGEST_DOMAIN);
    let root_metadata =
        fs::symlink_metadata(root).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
    if root_metadata.file_type().is_symlink()
        || !root_metadata.is_dir()
        || root_metadata.permissions().mode() & 0o222 != 0
    {
        return Err(ProcessAdapterError::InvalidSandbox);
    }
    hasher.update(b"R");
    hasher.update((root_metadata.permissions().mode() & 0o777).to_be_bytes());
    let mut total_bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1_024];

    for relative in relative_paths {
        let path = root.join(&relative);
        let metadata =
            fs::symlink_metadata(&path).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
        let path_bytes = relative.as_os_str().as_encoded_bytes();
        let path_length =
            u32::try_from(path_bytes.len()).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
        hasher.update(path_length.to_be_bytes());
        hasher.update(path_bytes);
        hasher.update((metadata.permissions().mode() & 0o777).to_be_bytes());

        if metadata.file_type().is_dir() {
            if metadata.permissions().mode() & 0o222 != 0 {
                return Err(ProcessAdapterError::InvalidSandbox);
            }
            hasher.update(b"D");
            continue;
        }
        if !metadata.file_type().is_file()
            || metadata.nlink() != 1
            || metadata.permissions().mode() & 0o222 != 0
            || metadata.len() > MAX_ROOT_FILESYSTEM_FILE_BYTES
        {
            return Err(ProcessAdapterError::InvalidSandbox);
        }

        total_bytes = total_bytes
            .checked_add(metadata.len())
            .filter(|total| *total <= MAX_ROOT_FILESYSTEM_TOTAL_BYTES)
            .ok_or(ProcessAdapterError::InvalidSandbox)?;
        hasher.update(b"F");
        hasher.update(metadata.len().to_be_bytes());

        let mut file = File::open(&path).map_err(|_| ProcessAdapterError::InvalidSandbox)?;
        let opened = file
            .metadata()
            .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
        if opened.dev() != metadata.dev()
            || opened.ino() != metadata.ino()
            || opened.len() != metadata.len()
            || opened.nlink() != 1
            || opened.permissions().mode() & 0o222 != 0
        {
            return Err(ProcessAdapterError::InvalidSandbox);
        }
        let mut read_bytes = 0_u64;
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
            if read == 0 {
                break;
            }
            read_bytes = read_bytes
                .checked_add(u64::try_from(read).map_err(|_| ProcessAdapterError::InvalidSandbox)?)
                .ok_or(ProcessAdapterError::InvalidSandbox)?;
            if read_bytes > metadata.len() {
                return Err(ProcessAdapterError::InvalidSandbox);
            }
            hasher.update(&buffer[..read]);
        }
        if read_bytes != metadata.len() {
            return Err(ProcessAdapterError::InvalidSandbox);
        }
    }

    let digest: [u8; DIGEST_BYTES] = hasher.finalize().into();
    Ok(RootFilesystemDigest(digest))
}

#[cfg(not(unix))]
fn measure_tree(_root: &Path) -> Result<RootFilesystemDigest, ProcessAdapterError> {
    Err(ProcessAdapterError::UnsupportedPlatform)
}

fn collect_relative_paths(root: &Path) -> Result<Vec<PathBuf>, ProcessAdapterError> {
    let mut directories = vec![PathBuf::new()];
    let mut paths = Vec::new();

    while let Some(relative_directory) = directories.pop() {
        let directory = root.join(&relative_directory);
        for entry in fs::read_dir(directory).map_err(|_| ProcessAdapterError::InvalidSandbox)? {
            let entry = entry.map_err(|_| ProcessAdapterError::InvalidSandbox)?;
            let relative = relative_directory.join(entry.file_name());
            if relative.as_os_str().as_encoded_bytes().len() > super::MAX_SANDBOX_PATH_BYTES {
                return Err(ProcessAdapterError::InvalidSandbox);
            }
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|_| ProcessAdapterError::InvalidSandbox)?;
            if metadata.file_type().is_symlink()
                || (!metadata.file_type().is_dir() && !metadata.file_type().is_file())
            {
                return Err(ProcessAdapterError::InvalidSandbox);
            }
            paths.push(relative.clone());
            if paths.len() > MAX_ROOT_FILESYSTEM_ENTRIES {
                return Err(ProcessAdapterError::InvalidSandbox);
            }
            if metadata.file_type().is_dir() {
                directories.push(relative);
            }
        }
    }
    Ok(paths)
}

#[cfg(all(test, unix))]
mod tests {
    use std::fs::{self, DirBuilder, OpenOptions};
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt, symlink};
    use std::path::{Path, PathBuf};

    use aios_runtime::TaskId;

    use super::{
        MAX_ROOT_FILESYSTEM_FILE_BYTES, RootFilesystemDigest, VerifiedRootFilesystem,
        build_minimal_root_filesystem, validate_verified_root_filesystem,
    };
    use crate::ProcessAdapterError;

    struct RootFixture {
        base: PathBuf,
        root: PathBuf,
    }

    impl RootFixture {
        fn new() -> Self {
            let base = std::env::temp_dir().join(format!("aios-rootfs-{}", TaskId::new()));
            let mut builder = DirBuilder::new();
            builder.mode(0o700);
            builder.create(&base).expect("create rootfs fixture");
            let root = base.join("root");
            for directory in ["bin", "proc", "dev", "tmp", "workspace"] {
                fs::create_dir_all(root.join(directory)).expect("create rootfs directory");
            }
            fs::write(root.join("bin/tool"), b"verified-tool-v1").expect("write rootfs executable");
            fs::set_permissions(root.join("bin/tool"), fs::Permissions::from_mode(0o555))
                .expect("make rootfs executable read-only");
            seal_tree(&root);
            Self { base, root }
        }

        fn tool(&self) -> PathBuf {
            self.root.join("bin/tool")
        }
    }

    impl Drop for RootFixture {
        fn drop(&mut self) {
            make_tree_removable(&self.base);
            let _result = fs::remove_dir_all(&self.base);
        }
    }

    #[test]
    fn rootfs_digest_is_canonical_and_detects_content_changes() {
        let fixture = RootFixture::new();
        let digest = RootFilesystemDigest::measure(&fixture.root).expect("measure rootfs");
        let repeated = RootFilesystemDigest::measure(&fixture.root).expect("remeasure rootfs");
        assert!(digest == repeated);
        let encoded = digest.to_string();
        assert_eq!(encoded.len(), 64);
        assert!(
            encoded
                .parse::<RootFilesystemDigest>()
                .expect("parse digest")
                == digest
        );
        assert!("A".repeat(64).parse::<RootFilesystemDigest>().is_err());

        let verified = VerifiedRootFilesystem::open(&fixture.root, digest).expect("verify rootfs");
        fs::set_permissions(fixture.tool(), fs::Permissions::from_mode(0o755))
            .expect("make rootfs executable writable");
        fs::write(fixture.tool(), b"modified-tool-v1").expect("modify rootfs executable");
        fs::set_permissions(fixture.tool(), fs::Permissions::from_mode(0o555))
            .expect("reseal rootfs executable");

        assert!(matches!(
            validate_verified_root_filesystem(verified.directory(), verified.state()),
            Err(ProcessAdapterError::InvalidSandbox)
        ));
    }

    #[test]
    fn minimal_rootfs_builder_is_create_new_sealed_and_reproducible() {
        let fixture = private_builder_fixture();
        let source = fixture.join("busybox-source");
        fs::write(&source, b"static-busybox-probe").expect("write BusyBox source");
        fs::set_permissions(&source, fs::Permissions::from_mode(0o755))
            .expect("make BusyBox source executable");
        let first_root = fixture.join("root-one");
        let second_root = fixture.join("root-two");

        let first =
            build_minimal_root_filesystem(&source, &first_root).expect("build first rootfs");
        let second =
            build_minimal_root_filesystem(&source, &second_root).expect("build second rootfs");

        assert!(first == second);
        assert_eq!(
            fs::metadata(first_root.join("bin/busybox"))
                .expect("read built BusyBox metadata")
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
        assert_eq!(
            fs::metadata(&first_root)
                .expect("read built root metadata")
                .permissions()
                .mode()
                & 0o777,
            0o555
        );
        assert!(VerifiedRootFilesystem::open(&first_root, first).is_ok());
        assert!(matches!(
            build_minimal_root_filesystem(&source, &first_root),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        make_tree_removable(&fixture);
        fs::remove_dir_all(fixture).expect("remove builder fixture");
    }

    #[test]
    fn rootfs_rejects_writable_symlinked_and_replaced_trees() {
        let fixture = RootFixture::new();
        let digest = RootFilesystemDigest::measure(&fixture.root).expect("measure rootfs");
        let verified = VerifiedRootFilesystem::open(&fixture.root, digest).expect("verify rootfs");

        fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o755))
            .expect("make rootfs writable");
        assert!(matches!(
            RootFilesystemDigest::measure(&fixture.root),
            Err(ProcessAdapterError::InvalidSandbox)
        ));
        fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o555))
            .expect("reseal rootfs");

        let symlink_root = fixture.base.join("root-link");
        symlink(&fixture.root, &symlink_root).expect("create rootfs symlink");
        assert!(matches!(
            RootFilesystemDigest::measure(&symlink_root),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        let moved = fixture.base.join("moved-root");
        fs::rename(&fixture.root, &moved).expect("move verified rootfs");
        fs::create_dir(&fixture.root).expect("create replacement rootfs");
        fs::set_permissions(&fixture.root, fs::Permissions::from_mode(0o555))
            .expect("seal replacement rootfs");
        assert!(matches!(
            validate_verified_root_filesystem(&fixture.root, verified.state()),
            Err(ProcessAdapterError::InvalidSandbox)
        ));
    }

    #[test]
    fn rootfs_rejects_special_entries_missing_mounts_and_oversized_files() {
        let fixture = RootFixture::new();
        make_tree_writable(&fixture.root);
        symlink("/etc/passwd", fixture.root.join("bin/escape"))
            .expect("create rootfs entry symlink");
        seal_tree(&fixture.root);
        assert!(matches!(
            RootFilesystemDigest::measure(&fixture.root),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        make_tree_writable(&fixture.root);
        fs::remove_file(fixture.root.join("bin/escape")).expect("remove rootfs symlink");
        let hardlink_source = fixture.root.join("bin/hardlink-source");
        let hardlink_alias = fixture.root.join("bin/hardlink-alias");
        fs::write(&hardlink_source, b"linked").expect("write hardlink source");
        fs::hard_link(&hardlink_source, &hardlink_alias).expect("create rootfs hardlink");
        seal_tree(&fixture.root);
        assert!(matches!(
            RootFilesystemDigest::measure(&fixture.root),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        make_tree_writable(&fixture.root);
        fs::remove_file(hardlink_alias).expect("remove hardlink alias");
        fs::remove_file(hardlink_source).expect("remove hardlink source");
        fs::remove_dir(fixture.root.join("proc")).expect("remove mount point");
        seal_tree(&fixture.root);
        let digest =
            RootFilesystemDigest::measure(&fixture.root).expect("measure incomplete rootfs");
        assert!(matches!(
            VerifiedRootFilesystem::open(&fixture.root, digest),
            Err(ProcessAdapterError::InvalidSandbox)
        ));

        make_tree_writable(&fixture.root);
        fs::create_dir(fixture.root.join("proc")).expect("restore mount point");
        let oversized = fixture.root.join("bin/oversized");
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&oversized)
            .expect("create oversized rootfs file");
        file.write_all(b"x").expect("seed oversized file");
        file.set_len(MAX_ROOT_FILESYSTEM_FILE_BYTES + 1)
            .expect("extend sparse oversized file");
        drop(file);
        seal_tree(&fixture.root);
        assert!(matches!(
            RootFilesystemDigest::measure(&fixture.root),
            Err(ProcessAdapterError::InvalidSandbox)
        ));
    }

    fn seal_tree(path: &Path) {
        let metadata = fs::symlink_metadata(path).expect("read tree entry");
        if metadata.file_type().is_symlink() {
            return;
        }
        if metadata.is_dir() {
            for entry in fs::read_dir(path).expect("read tree directory") {
                seal_tree(&entry.expect("read tree entry").path());
            }
        }
        let mode = metadata.permissions().mode() & 0o777 & !0o222;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("seal tree entry");
    }

    fn make_tree_writable(path: &Path) {
        let metadata = fs::symlink_metadata(path).expect("read tree entry");
        if metadata.file_type().is_symlink() {
            return;
        }
        let mode = metadata.permissions().mode() & 0o777 | 0o200;
        fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("open tree entry");
        if metadata.is_dir() {
            for entry in fs::read_dir(path).expect("read tree directory") {
                make_tree_writable(&entry.expect("read tree entry").path());
            }
        }
    }

    fn make_tree_removable(path: &Path) {
        if !path.exists() {
            return;
        }
        make_tree_writable(path);
    }

    fn private_builder_fixture() -> PathBuf {
        let directory = std::env::temp_dir().join(format!("aios-rootfs-build-{}", TaskId::new()));
        let mut builder = DirBuilder::new();
        builder.mode(0o700);
        builder.create(&directory).expect("create builder fixture");
        directory
    }
}
