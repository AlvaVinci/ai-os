use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::fs::File;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use aios_core::TaskSpec;
use aios_local_api::{
    ApiMethod, ApiOutcome, ApiRequest, DEFAULT_EVENT_PAGE_SIZE, DEFAULT_MAX_FRAME_BYTES,
    LocalApiError, ServerConfig, send_request,
};
use aios_runtime::TaskId;

const USAGE: &str =
    "usage: aiosctl --socket PATH <health|submit|get|events|start|succeed|fail|cancel> [ARGUMENTS]";

fn main() -> ExitCode {
    match run(std::env::args().skip(1)) {
        Ok(response) => {
            let is_error = matches!(&response.outcome, ApiOutcome::Error { .. });
            match serde_json::to_string_pretty(&response) {
                Ok(json) => println!("{json}"),
                Err(_) => {
                    eprintln!("failed to encode local API response");
                    return ExitCode::FAILURE;
                }
            }
            if is_error {
                ExitCode::from(2)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(error) => {
            eprintln!("{error}");
            if matches!(error, CliError::Transport(_)) {
                ExitCode::FAILURE
            } else {
                ExitCode::from(2)
            }
        }
    }
}

fn run(arguments: impl Iterator<Item = String>) -> Result<aios_local_api::ApiResponse, CliError> {
    let options = Options::parse(arguments)?;
    let method = options.command.into_method()?;
    send_request(
        options.socket,
        &ApiRequest::new(method),
        ServerConfig::default(),
    )
    .map_err(CliError::Transport)
}

struct Options {
    socket: PathBuf,
    command: Command,
}

impl Options {
    fn parse(arguments: impl Iterator<Item = String>) -> Result<Self, CliError> {
        let mut arguments = arguments;
        if arguments.next().as_deref() != Some("--socket") {
            return Err(CliError::Usage);
        }
        let socket = arguments.next().filter(|value| !value.is_empty());
        let command = arguments.next().ok_or(CliError::Usage)?;
        let values: Vec<String> = arguments.collect();

        Ok(Self {
            socket: PathBuf::from(socket.ok_or(CliError::Usage)?),
            command: Command::parse(&command, &values)?,
        })
    }
}

enum Command {
    Health,
    Submit(PathBuf),
    Get(TaskId),
    Events {
        task_id: TaskId,
        after_sequence: u64,
        limit: u16,
    },
    Start(TaskId),
    Succeed(TaskId),
    Fail(TaskId),
    Cancel(TaskId),
}

impl Command {
    fn parse(name: &str, values: &[String]) -> Result<Self, CliError> {
        match (name, values) {
            ("health", []) => Ok(Self::Health),
            ("submit", [path]) => Ok(Self::Submit(PathBuf::from(path))),
            ("get", [task_id]) => Ok(Self::Get(parse_task_id(task_id)?)),
            ("events", [task_id]) => Ok(Self::Events {
                task_id: parse_task_id(task_id)?,
                after_sequence: 0,
                limit: DEFAULT_EVENT_PAGE_SIZE,
            }),
            ("events", [task_id, after_sequence]) => Ok(Self::Events {
                task_id: parse_task_id(task_id)?,
                after_sequence: parse_number(after_sequence)?,
                limit: DEFAULT_EVENT_PAGE_SIZE,
            }),
            ("events", [task_id, after_sequence, limit]) => Ok(Self::Events {
                task_id: parse_task_id(task_id)?,
                after_sequence: parse_number(after_sequence)?,
                limit: parse_number(limit)?,
            }),
            ("start", [task_id]) => Ok(Self::Start(parse_task_id(task_id)?)),
            ("succeed", [task_id]) => Ok(Self::Succeed(parse_task_id(task_id)?)),
            ("fail", [task_id]) => Ok(Self::Fail(parse_task_id(task_id)?)),
            ("cancel", [task_id]) => Ok(Self::Cancel(parse_task_id(task_id)?)),
            _ => Err(CliError::Usage),
        }
    }

    fn into_method(self) -> Result<ApiMethod, CliError> {
        Ok(match self {
            Self::Health => ApiMethod::Health {},
            Self::Submit(path) => ApiMethod::Submit {
                task: Box::new(load_task(&path)?),
            },
            Self::Get(task_id) => ApiMethod::GetTask { task_id },
            Self::Events {
                task_id,
                after_sequence,
                limit,
            } => ApiMethod::Events {
                task_id,
                after_sequence,
                limit,
            },
            Self::Start(task_id) => ApiMethod::Start { task_id },
            Self::Succeed(task_id) => ApiMethod::Succeed { task_id },
            Self::Fail(task_id) => ApiMethod::Fail { task_id },
            Self::Cancel(task_id) => ApiMethod::Cancel { task_id },
        })
    }
}

fn parse_task_id(value: &str) -> Result<TaskId, CliError> {
    value.parse().map_err(|_| CliError::InvalidTaskId)
}

fn parse_number<T: std::str::FromStr>(value: &str) -> Result<T, CliError> {
    value.parse().map_err(|_| CliError::InvalidNumber)
}

fn load_task(path: &Path) -> Result<TaskSpec, CliError> {
    let file = File::open(path).map_err(CliError::TaskFileIo)?;
    let limit = u64::try_from(DEFAULT_MAX_FRAME_BYTES).map_err(|_| CliError::TaskFileTooLarge)?;
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(CliError::TaskFileIo)?;
    if bytes.len() > DEFAULT_MAX_FRAME_BYTES {
        return Err(CliError::TaskFileTooLarge);
    }
    serde_json::from_slice(&bytes).map_err(|_| CliError::InvalidTaskFile)
}

#[derive(Debug)]
enum CliError {
    Usage,
    InvalidTaskId,
    InvalidNumber,
    TaskFileTooLarge,
    InvalidTaskFile,
    TaskFileIo(io::Error),
    Transport(LocalApiError),
}

impl Display for CliError {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::Usage => USAGE,
            Self::InvalidTaskId => "task ID is invalid",
            Self::InvalidNumber => "numeric argument is invalid",
            Self::TaskFileTooLarge => "task file exceeds the 65,536-byte limit",
            Self::InvalidTaskFile => "task file is not valid Task JSON",
            Self::TaskFileIo(_) => "task file could not be read",
            Self::Transport(_) => "local API request failed",
        };
        formatter.write_str(message)
    }
}

impl Error for CliError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::TaskFileIo(error) => Some(error),
            Self::Transport(error) => Some(error),
            Self::Usage
            | Self::InvalidTaskId
            | Self::InvalidNumber
            | Self::TaskFileTooLarge
            | Self::InvalidTaskFile => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};

    use super::{CliError, Command, Options, load_task};

    static NEXT_FILE: AtomicU64 = AtomicU64::new(1);

    struct TestFile(PathBuf);

    impl TestFile {
        fn with_bytes(bytes: &[u8]) -> Self {
            let sequence = NEXT_FILE.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("aiosctl-{}-{sequence}.json", std::process::id()));
            fs::write(&path, bytes).expect("write test file");
            Self(path)
        }
    }

    impl Drop for TestFile {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    #[test]
    fn parses_health_command() {
        let options = Options::parse(
            ["--socket", "/tmp/aios.sock", "health"]
                .into_iter()
                .map(str::to_owned),
        )
        .expect("valid command");

        assert!(matches!(options.command, Command::Health));
    }

    #[test]
    fn parses_events_defaults() {
        let options = Options::parse(
            [
                "--socket",
                "/tmp/aios.sock",
                "events",
                "00000000-0000-4000-8000-000000000001",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("valid command");

        assert!(matches!(
            options.command,
            Command::Events {
                after_sequence: 0,
                limit: 100,
                ..
            }
        ));
    }

    #[test]
    fn rejects_unknown_or_incomplete_commands() {
        assert!(matches!(
            Options::parse(
                ["--socket", "/tmp/aios.sock", "unknown"]
                    .into_iter()
                    .map(str::to_owned)
            ),
            Err(CliError::Usage)
        ));
    }

    #[test]
    fn loads_example_task() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/task.json");
        load_task(&path).expect("valid example task");
    }

    #[test]
    fn rejects_oversized_task_before_json_parsing() {
        let file = TestFile::with_bytes(&vec![b' '; 65_537]);
        assert!(matches!(
            load_task(&file.0),
            Err(CliError::TaskFileTooLarge)
        ));
    }
}
