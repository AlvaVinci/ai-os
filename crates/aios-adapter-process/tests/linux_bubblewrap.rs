#![cfg(target_os = "linux")]

use std::env;
use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};
use std::net::TcpListener;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixListener;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use aios_adapter_process::{BubblewrapProcessToolBuilder, ProcessAdapterError, ProcessToolBuilder};

const TEST_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HTTP_REQUEST_BYTES: usize = 8 * 1_024;

struct SandboxFixture {
    base: PathBuf,
    bubblewrap: PathBuf,
    busybox: PathBuf,
    root_filesystem: PathBuf,
    scratch_directory: PathBuf,
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
        fs::create_dir(&base).expect("create unique sandbox fixture");

        let root_filesystem = base.join("rootfs");
        let scratch_directory = base.join("scratch");
        let host_directory = base.join("host-only");
        for directory in [
            root_filesystem.join("bin"),
            root_filesystem.join("proc"),
            root_filesystem.join("dev"),
            root_filesystem.join("tmp"),
            root_filesystem.join("workspace"),
            scratch_directory.clone(),
            host_directory.clone(),
        ] {
            fs::create_dir_all(directory).expect("create sandbox fixture directory");
        }

        let bubblewrap = required_executable("AIOS_BWRAP_PATH");
        let busybox = required_executable("AIOS_BUSYBOX_PATH");
        let sandbox_busybox = root_filesystem.join("bin/busybox");
        fs::copy(&busybox, &sandbox_busybox).expect("copy static BusyBox into rootfs");
        fs::set_permissions(&sandbox_busybox, fs::Permissions::from_mode(0o755))
            .expect("make sandbox BusyBox executable");

        Self {
            base,
            bubblewrap,
            busybox,
            root_filesystem,
            scratch_directory,
            host_directory,
        }
    }

    fn run_sandbox(
        &self,
        fixed_arguments: &[&str],
        dynamic_arguments: Vec<String>,
    ) -> Result<(), ProcessAdapterError> {
        let mut handler = BubblewrapProcessToolBuilder::new(
            &self.bubblewrap,
            &self.root_filesystem,
            "/bin/busybox",
            &self.scratch_directory,
            |_: &[String]| true,
        )
        .fixed_arguments(
            fixed_arguments
                .iter()
                .map(|argument| (*argument).to_owned())
                .collect(),
        )
        .timeout(TEST_TIMEOUT)
        .build()
        .expect("build Bubblewrap handler");

        handler.run_checked(dynamic_arguments).map(|_| ())
    }
}

impl Drop for SandboxFixture {
    fn drop(&mut self) {
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
    assert!(fixture.scratch_directory.join("created").is_file());

    assert!(matches!(
        fixture.run_sandbox(&["touch"], vec!["/root-write-must-fail".to_owned()]),
        Err(ProcessAdapterError::ExitFailed)
    ));
    assert!(
        !fixture
            .root_filesystem
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

    let output = fixture.scratch_directory.join("direct-network-output");
    let mut handler = ProcessToolBuilder::new(
        &fixture.busybox,
        &fixture.scratch_directory,
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
