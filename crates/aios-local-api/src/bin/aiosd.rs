use std::error::Error;
use std::path::PathBuf;

use aios_local_api::{ApiService, LocalServer, ServerConfig};
use aios_runtime::TaskSupervisor;
use aios_storage_sqlite::SqliteEventStore;

const DEFAULT_MAX_EVENTS_PER_TASK: usize = 10_000;
const DEFAULT_MAX_TASKS: usize = 10_000;

fn main() -> Result<(), Box<dyn Error>> {
    let options = Options::parse(std::env::args().skip(1))?;
    let server = LocalServer::bind(&options.socket, ServerConfig::default())?;
    let store = SqliteEventStore::open(&options.database, DEFAULT_MAX_EVENTS_PER_TASK)?;
    let supervisor = TaskSupervisor::recover(store, DEFAULT_MAX_TASKS)?;
    let mut service = ApiService::new(supervisor);

    eprintln!("aiosd listening on {:?}", options.socket);
    server.serve_forever(&mut service)?;
    Ok(())
}

struct Options {
    socket: PathBuf,
    database: PathBuf,
}

impl Options {
    fn parse(mut arguments: impl Iterator<Item = String>) -> Result<Self, OptionsError> {
        let mut socket = None;
        let mut database = None;

        while let Some(argument) = arguments.next() {
            let value = arguments.next().ok_or(OptionsError)?;
            match argument.as_str() {
                "--socket" if socket.is_none() => socket = Some(PathBuf::from(value)),
                "--database" if database.is_none() => database = Some(PathBuf::from(value)),
                _ => return Err(OptionsError),
            }
        }

        Ok(Self {
            socket: socket.ok_or(OptionsError)?,
            database: database.ok_or(OptionsError)?,
        })
    }
}

#[derive(Debug)]
struct OptionsError;

impl std::fmt::Display for OptionsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("usage: aiosd --socket PATH --database PATH")
    }
}

impl Error for OptionsError {}

#[cfg(test)]
mod tests {
    use super::Options;

    #[test]
    fn parses_required_paths() {
        let options = Options::parse(
            [
                "--socket",
                "/tmp/aios/aiosd.sock",
                "--database",
                "/tmp/aios/events.sqlite",
            ]
            .into_iter()
            .map(str::to_owned),
        )
        .expect("valid options");

        assert_eq!(options.socket.to_string_lossy(), "/tmp/aios/aiosd.sock");
        assert_eq!(
            options.database.to_string_lossy(),
            "/tmp/aios/events.sqlite"
        );
    }

    #[test]
    fn rejects_missing_values() {
        assert!(Options::parse(["--socket"].into_iter().map(str::to_owned)).is_err());
    }

    #[test]
    fn rejects_unknown_or_duplicate_options() {
        assert!(
            Options::parse(
                ["--socket", "/tmp/a", "--socket", "/tmp/b"]
                    .into_iter()
                    .map(str::to_owned)
            )
            .is_err()
        );
    }
}
