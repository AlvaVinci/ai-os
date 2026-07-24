#![cfg(target_os = "linux")]

use std::env;
use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aios_adapter_process::{
    BubblewrapProcessToolBuilder, CgroupResourceBudget, CgroupV2Manager, ProcessAdapterError,
    ProcessToolBuilder, ProcessToolHandler, TaskCgroup, TaskScratch, TaskScratchManager,
    VerifiedRootFilesystem, build_minimal_root_filesystem,
};
use aios_runtime::TaskId;

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HTTP_REQUEST_BYTES: usize = 8 * 1_024;
const DESCENDANT_TIMEOUT: Duration = Duration::from_secs(1);
const DESCENDANT_OBSERVATION_DELAY: Duration = Duration::from_millis(2_500);
const MEBIBYTE: u64 = 1_024 * 1_024;
const EXITING_PARENT_SCRIPT: &str = "(touch /workspace/exit-descendant-started; sleep 2; touch /workspace/exit-descendant-survived) & while [ ! -e /workspace/exit-descendant-started ]; do :; done";
const TIMED_OUT_PARENT_SCRIPT: &str = "(touch /workspace/timeout-descendant-started; sleep 2; touch /workspace/timeout-descendant-survived) & while [ ! -e /workspace/timeout-descendant-started ]; do :; done; sleep 10";

fn cgroup_launcher() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_aios-cgroup-launch"))
}

struct SandboxFixture {
    base: PathBuf,
    bubblewrap: PathBuf,
    busybox: PathBuf,
    root_filesystem: VerifiedRootFilesystem,
    task_scratch: TaskScratch,
    host_directory: PathBuf,
}

impl SandboxFixture {
    fn new(label: &str) -> Self {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time must follow Unix epoch")
            .as_nanos();
        let base = env::temp_dir().join(format!(
            "aios-bubblewrap-{label}-{}-{nonce}",
            std::process::id()
        ));
        let mut directory_builder = DirBuilder::new();
        directory_builder.mode(0o700);
        directory_builder
            .create(&base)
            .expect("create unique sandbox fixture");

        let root_filesystem = base.join("rootfs");
        let scratch_root = base.join("tasks");
        let host_directory = base.join("host-only");
        fs::create_dir(&host_directory).expect("create host-only fixture directory");
        directory_builder
            .create(&scratch_root)
            .expect("create Task scratch root");
        let task_scratch = TaskScratchManager::new(&scratch_root)
            .expect("open Task scratch root")
            .create(TaskId::new())
            .expect("create Task scratch directory");

        let bubblewrap = required_executable("AIOS_BWRAP_PATH");
        let busybox = required_executable("AIOS_BUSYBOX_PATH");
        let root_digest = build_minimal_root_filesystem(&busybox, &root_filesystem)
            .expect("build sealed minimal rootfs");
        let root_filesystem = VerifiedRootFilesystem::open(&root_filesystem, root_digest)
            .expect("verify sealed rootfs");

        Self {
            base,
            bubblewrap,
            busybox,
            root_filesystem,
            task_scratch,
            host_directory,
        }
    }

    fn run_sandbox(
        &self,
        fixed_arguments: &[&str],
        dynamic_arguments: Vec<String>,
    ) -> Result<(), ProcessAdapterError> {
        self.run_sandbox_with_timeout(fixed_arguments, dynamic_arguments, TEST_TIMEOUT)
    }

    fn run_sandbox_with_timeout(
        &self,
        fixed_arguments: &[&str],
        dynamic_arguments: Vec<String>,
        timeout: Duration,
    ) -> Result<(), ProcessAdapterError> {
        let mut handler = self.sandbox_handler(fixed_arguments, timeout);

        handler.run_checked(dynamic_arguments).map(|_| ())
    }

    fn sandbox_handler(&self, fixed_arguments: &[&str], timeout: Duration) -> ProcessToolHandler {
        self.sandbox_builder(fixed_arguments, timeout)
            .build()
            .expect("build Bubblewrap handler")
    }

    fn sandbox_builder(
        &self,
        fixed_arguments: &[&str],
        timeout: Duration,
    ) -> BubblewrapProcessToolBuilder {
        BubblewrapProcessToolBuilder::new_for_verified_task(
            &self.bubblewrap,
            &self.root_filesystem,
            "/bin/busybox",
            &self.task_scratch,
            |_: &[String]| true,
        )
        .fixed_arguments(
            fixed_arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
        )
        .timeout(timeout)
    }

    fn create_task_cgroup(&self, budget: CgroupResourceBudget) -> TaskCgroup {
        let root = env::var_os("AIOS_CGROUP_ROOT")
            .map(PathBuf::from)
            .expect("delegated cgroup root must be configured");
        CgroupV2Manager::new(root)
            .expect("open delegated cgroup v2 root")
            .create(self.task_scratch.task_id(), budget)
            .expect("create Task cgroup")
    }
}

impl Drop for SandboxFixture {
    fn drop(&mut self) {
        make_tree_writable(&self.base);
        let _result = fs::remove_dir_all(&self.base);
    }
}

fn required_executable(variable: &str) -> PathBuf {
    let path =
        PathBuf::from(env::var_os(variable).expect("required test executable is configured"));
    assert!(path.is_absolute(), "test executable must be absolute");
    assert!(path.is_file(), "test executable must exist");
    path
}

#[test]
#[ignore = "requires Linux Bubblewrap and static BusyBox"]
fn linux_sandbox_enforces_filesystem_socket_and_descriptor_boundaries() {
    let fixture = SandboxFixture::new("host-boundaries");

    fixture
        .run_sandbox(&["touch"], vec!["/workspace/created".to_owned()])
        .expect("scratch must be writable");
    assert!(fixture.task_scratch.directory().join("created").is_file());

    assert!(matches!(
        fixture.run_sandbox(&["touch"], vec!["/root-write-must-fail".to_owned()]),
        Err(ProcessAdapterError::ExitFailed)
    ));
    assert!(
        !fixture
            .root_filesystem
            .directory()
            .join("root-write-must-fail")
            .exists()
    );

    let control_socket_path = fixture.host_directory.join("approval.sock");
    let _control_listener =
        UnixListener::bind(&control_socket_path).expect("bind host-only control socket");
    fixture
        .run_sandbox(
            &["test", "!", "-e"],
            vec![path_string(&control_socket_path)],
        )
        .expect("host control socket must be invisible");

    let descriptor_path = fixture.host_directory.join("inherited-descriptor");
    let inherited_file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(descriptor_path)
        .expect("open host descriptor");
    let child_descriptor_path = format!("/proc/self/fd/{}", inherited_file.as_raw_fd());
    fixture
        .run_sandbox(&["test", "!", "-e"], vec![child_descriptor_path])
        .expect("non-standard host descriptor must be closed before Tool execution");
}

#[test]
#[ignore = "requires Linux Bubblewrap and static BusyBox"]
fn linux_sandbox_terminates_descendants_after_initial_process_exit() {
    let fixture = SandboxFixture::new("exit-descendant-cleanup");

    fixture
        .run_sandbox(&["sh", "-c", EXITING_PARENT_SCRIPT], Vec::new())
        .expect("initial sandbox process must exit successfully");
    assert!(
        fixture
            .task_scratch
            .directory()
            .join("exit-descendant-started")
            .is_file(),
        "background descendant must start before the initial process exits"
    );

    thread::sleep(DESCENDANT_OBSERVATION_DELAY);
    assert!(
        !fixture
            .task_scratch
            .directory()
            .join("exit-descendant-survived")
            .exists(),
        "background descendant must not survive the initial process"
    );
}

#[test]
#[ignore = "requires Linux Bubblewrap and static BusyBox"]
fn linux_sandbox_terminates_descendants_after_timeout() {
    let fixture = SandboxFixture::new("timeout-descendant-cleanup");

    assert!(matches!(
        fixture.run_sandbox_with_timeout(
            &["sh", "-c", TIMED_OUT_PARENT_SCRIPT],
            Vec::new(),
            DESCENDANT_TIMEOUT,
        ),
        Err(ProcessAdapterError::TimedOut)
    ));
    assert!(
        fixture
            .task_scratch
            .directory()
            .join("timeout-descendant-started")
            .is_file(),
        "background descendant must start before timeout enforcement"
    );

    thread::sleep(DESCENDANT_OBSERVATION_DELAY);
    assert!(
        !fixture
            .task_scratch
            .directory()
            .join("timeout-descendant-survived")
            .exists(),
        "background descendant must not survive timeout enforcement"
    );
}

#[test]
#[ignore = "requires Linux Bubblewrap and static BusyBox"]
fn linux_sandbox_blocks_host_network() {
    let fixture = SandboxFixture::new("network-boundary");
    verify_busybox_wget_can_reach_host(&fixture);

    fixture
        .run_sandbox(&["true"], Vec::new())
        .expect("sandbox must start before testing network denial");

    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind host-only TCP listener");
    listener
        .set_nonblocking(true)
        .expect("make host listener nonblocking");
    let address = listener.local_addr().expect("read host listener address");
    let result = fixture.run_sandbox(
        &["wget", "-q", "-O", "/workspace/network-output"],
        vec![format!("http://{address}/")],
    );

    assert!(matches!(result, Err(ProcessAdapterError::ExitFailed)));
    assert!(matches!(listener.accept(), Err(error) if error.kind() == io::ErrorKind::WouldBlock));
}

#[test]
#[ignore = "requires Linux Bubblewrap and static BusyBox"]
fn linux_sandbox_rejects_rootfs_content_change_before_spawn() {
    let fixture = SandboxFixture::new("rootfs-tamper");
    let mut handler = fixture.sandbox_handler(&["true"], TEST_TIMEOUT);
    let sandbox_busybox = fixture.root_filesystem.directory().join("bin/busybox");
    fs::set_permissions(&sandbox_busybox, fs::Permissions::from_mode(0o755))
        .expect("make sandbox BusyBox writable");
    OpenOptions::new()
        .write(true)
        .open(&sandbox_busybox)
        .expect("open sandbox BusyBox")
        .write_all(b"X")
        .expect("modify sandbox BusyBox");
    fs::set_permissions(&sandbox_busybox, fs::Permissions::from_mode(0o555))
        .expect("reseal sandbox BusyBox");

    assert!(matches!(
        handler.run_checked(Vec::new()),
        Err(ProcessAdapterError::InvalidSandbox)
    ));
}

#[test]
#[ignore = "requires Linux Bubblewrap, static BusyBox, and a delegated cgroup v2 subtree"]
fn linux_sandbox_stops_at_cumulative_cpu_time_limit() {
    let fixture = SandboxFixture::new("cpu-budget");
    let task_cgroup = fixture.create_task_cgroup(
        CgroupResourceBudget::new(Duration::from_millis(100), 256 * MEBIBYTE)
            .expect("configure CPU budget"),
    );
    let cgroup_directory = task_cgroup.directory().to_owned();
    let mut handler = fixture
        .sandbox_builder(&["sh", "-c", "while :; do :; done"], TEST_TIMEOUT)
        .task_cgroup(&task_cgroup, cgroup_launcher())
        .build()
        .expect("build cgroup-controlled handler");

    assert!(matches!(
        handler.run_checked(Vec::new()),
        Err(ProcessAdapterError::ResourceLimitExceeded)
    ));

    drop(handler);
    task_cgroup.finish().expect("remove CPU-limited cgroup");
    assert!(!cgroup_directory.exists());
}

#[test]
#[ignore = "requires Linux Bubblewrap, static BusyBox, and a delegated cgroup v2 subtree"]
fn linux_sandbox_stops_at_resident_memory_limit() {
    let fixture = SandboxFixture::new("memory-budget");
    let task_cgroup = fixture.create_task_cgroup(
        CgroupResourceBudget::new(Duration::from_secs(5), 32 * MEBIBYTE)
            .expect("configure memory budget"),
    );
    let cgroup_directory = task_cgroup.directory().to_owned();
    let mut handler = fixture
        .sandbox_builder(
            &[
                "dd",
                "if=/dev/zero",
                "of=/tmp/memory-fill",
                "bs=1048576",
                "count=256",
            ],
            TEST_TIMEOUT,
        )
        .task_cgroup(&task_cgroup, cgroup_launcher())
        .build()
        .expect("build cgroup-controlled handler");

    assert!(matches!(
        handler.run_checked(Vec::new()),
        Err(ProcessAdapterError::ResourceLimitExceeded)
    ));

    drop(handler);
    task_cgroup.finish().expect("remove memory-limited cgroup");
    assert!(!cgroup_directory.exists());
}

#[test]
#[ignore = "requires Linux Bubblewrap, static BusyBox, and a delegated cgroup v2 subtree"]
fn linux_sandbox_rejects_mismatched_or_reused_task_cgroup() {
    let fixture = SandboxFixture::new("cgroup-identity");
    let root = env::var_os("AIOS_CGROUP_ROOT")
        .map(PathBuf::from)
        .expect("delegated cgroup root must be configured");
    let manager = CgroupV2Manager::new(root).expect("open delegated cgroup v2 root");
    let budget = CgroupResourceBudget::new(Duration::from_secs(1), 64 * MEBIBYTE)
        .expect("configure resource budget");
    let other_task = TaskId::new();
    let task_cgroup = manager
        .create(other_task, budget)
        .expect("create mismatched Task cgroup");

    assert!(matches!(
        fixture
            .sandbox_builder(&["true"], TEST_TIMEOUT)
            .task_cgroup(&task_cgroup, cgroup_launcher())
            .build(),
        Err(ProcessAdapterError::InvalidResourceControl)
    ));
    assert!(matches!(
        manager.create(other_task, budget),
        Err(ProcessAdapterError::InvalidResourceControl)
    ));

    task_cgroup.finish().expect("remove mismatched Task cgroup");
}

fn verify_busybox_wget_can_reach_host(fixture: &SandboxFixture) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind direct TCP listener");
    let address = listener.local_addr().expect("read direct listener address");
    let server = thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .expect("make direct listener nonblocking");
        let deadline = Instant::now() + TEST_TIMEOUT;
        loop {
            match listener.accept() {
                Ok((mut stream, _address)) => {
                    stream
                        .set_read_timeout(Some(TEST_TIMEOUT))
                        .expect("bound direct HTTP request timeout");
                    let mut request = [0_u8; MAX_HTTP_REQUEST_BYTES];
                    let mut request_bytes = 0_usize;
                    while request_bytes < request.len()
                        && !request[..request_bytes]
                            .windows(4)
                            .any(|window| window == b"\r\n\r\n")
                    {
                        let read = stream
                            .read(&mut request[request_bytes..])
                            .expect("read direct HTTP request");
                        assert!(read > 0, "direct HTTP request ended before headers");
                        request_bytes += read;
                    }
                    assert!(
                        request[..request_bytes]
                            .windows(4)
                            .any(|window| window == b"\r\n\r\n"),
                        "direct HTTP request headers exceeded the limit"
                    );
                    stream
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .expect("write direct HTTP response");
                    return;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "direct wget did not connect");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("direct listener failed: {error}"),
            }
        }
    });

    let output = fixture
        .task_scratch
        .directory()
        .join("direct-network-output");
    let mut handler = ProcessToolBuilder::new(
        &fixture.busybox,
        fixture.task_scratch.directory(),
        |arguments: &[String]| arguments.len() == 1,
    )
    .fixed_arguments(vec![
        "wget".to_owned(),
        "-q".to_owned(),
        "-O".to_owned(),
        path_string(&output),
    ])
    .timeout(TEST_TIMEOUT)
    .build()
    .expect("build direct BusyBox preflight");
    handler
        .run_checked(vec![format!("http://{address}/")])
        .expect("direct BusyBox wget must reach host listener");
    server.join().expect("join direct HTTP server");
    assert_eq!(fs::read(output).expect("read direct wget output"), b"ok");
}

fn path_string(path: &Path) -> String {
    path.to_str().expect("test path must be UTF-8").to_owned()
}

fn make_tree_writable(path: &Path) {
    let metadata = fs::symlink_metadata(path).expect("read fixture entry");
    if metadata.file_type().is_symlink() {
        return;
    }
    let mode = metadata.permissions().mode() & 0o777 | 0o200;
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).expect("open fixture entry");
    if metadata.is_dir() {
        for entry in fs::read_dir(path).expect("read fixture directory") {
            make_tree_writable(&entry.expect("read fixture entry").path());
        }
    }
}
