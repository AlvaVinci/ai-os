use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use aios_runtime::TaskId;

use super::{DirectoryIdentity, MAX_SANDBOX_PATH_BYTES, MAX_TIMEOUT, ProcessAdapterError};

const CGROUP2_MOUNT: &str = "/sys/fs/cgroup";
const CLEANUP_TIMEOUT: Duration = Duration::from_secs(5);
const CLEANUP_POLL_INTERVAL: Duration = Duration::from_millis(5);

/// Cumulative CPU-time and resident-memory ceilings for one Task cgroup.
#[derive(Clone, Copy)]
pub struct CgroupResourceBudget {
    cpu_time_micros: u64,
    memory_bytes: u64,
}

impl CgroupResourceBudget {
    pub fn new(cpu_time: Duration, memory_bytes: u64) -> Result<Self, ProcessAdapterError> {
        let cpu_time_micros =
            u64::try_from(cpu_time.as_micros()).map_err(|_| ProcessAdapterError::InvalidConfig)?;
        if cpu_time_micros == 0 || cpu_time > MAX_TIMEOUT || memory_bytes == 0 {
            return Err(ProcessAdapterError::InvalidConfig);
        }
        Ok(Self {
            cpu_time_micros,
            memory_bytes,
        })
    }

    #[must_use]
    pub fn cpu_time(&self) -> Duration {
        Duration::from_micros(self.cpu_time_micros)
    }

    #[must_use]
    pub fn memory_bytes(&self) -> u64 {
        self.memory_bytes
    }
}

/// Trusted owner of an existing delegated cgroup v2 subtree.
///
/// Host provisioning must enable the `cpu` and `memory` controllers, keep the delegated root
/// process-free, and start the runtime in a child cgroup beneath that root.
pub struct CgroupV2Manager {
    root_directory: PathBuf,
    root_identity: DirectoryIdentity,
}

impl CgroupV2Manager {
    pub fn new(root_directory: impl Into<PathBuf>) -> Result<Self, ProcessAdapterError> {
        if !cfg!(target_os = "linux") {
            return Err(ProcessAdapterError::UnsupportedPlatform);
        }
        let root_directory = canonical_cgroup_root(&root_directory.into())?;
        validate_cgroup_root(&root_directory)?;
        let root_identity = DirectoryIdentity::read(&root_directory)
            .map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
        Ok(Self {
            root_directory,
            root_identity,
        })
    }

    /// Creates a new process-free cgroup bound to `task_id`.
    pub fn create(
        &self,
        task_id: TaskId,
        budget: CgroupResourceBudget,
    ) -> Result<TaskCgroup, ProcessAdapterError> {
        self.validate_root()?;
        let directory = self.root_directory.join(format!("task-{task_id}"));
        if directory.as_os_str().as_encoded_bytes().len() > MAX_SANDBOX_PATH_BYTES {
            return Err(ProcessAdapterError::InvalidResourceControl);
        }
        fs::create_dir(&directory).map_err(|_| ProcessAdapterError::InvalidResourceControl)?;

        let configured = configure_task_cgroup(&directory, budget).and_then(|()| {
            let directory_identity = DirectoryIdentity::read(&directory)
                .map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
            Ok(TaskCgroup {
                task_id,
                directory: directory.clone(),
                directory_identity,
                budget,
            })
        });
        if configured.is_err() {
            let _result = fs::remove_dir(&directory);
        }
        configured
    }

    #[must_use]
    pub fn root_directory(&self) -> &Path {
        &self.root_directory
    }

    fn validate_root(&self) -> Result<(), ProcessAdapterError> {
        validate_cgroup_root(&self.root_directory)?;
        let identity = DirectoryIdentity::read(&self.root_directory)
            .map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
        if identity != self.root_identity {
            return Err(ProcessAdapterError::InvalidResourceControl);
        }
        Ok(())
    }
}

/// Task-scoped cgroup authority.
///
/// This value intentionally does not clean up on drop. Call [`finish`](Self::finish) only after
/// the Task no longer needs a Process Tool handler.
pub struct TaskCgroup {
    task_id: TaskId,
    directory: PathBuf,
    directory_identity: DirectoryIdentity,
    budget: CgroupResourceBudget,
}

impl TaskCgroup {
    #[must_use]
    pub fn task_id(&self) -> TaskId {
        self.task_id
    }

    #[must_use]
    pub fn directory(&self) -> &Path {
        &self.directory
    }

    #[must_use]
    pub fn budget(&self) -> CgroupResourceBudget {
        self.budget
    }

    /// Terminates any remaining processes and removes this dedicated Task cgroup.
    pub fn finish(self) -> Result<(), ProcessAdapterError> {
        let state = self.state();
        state.terminate()?;
        wait_until_empty(&state)?;
        fs::remove_dir(&self.directory).map_err(|_| ProcessAdapterError::InvalidResourceControl)
    }

    pub(crate) fn state(&self) -> TaskCgroupState {
        TaskCgroupState {
            task_id: self.task_id,
            directory: self.directory.clone(),
            directory_identity: self.directory_identity,
            budget: self.budget,
        }
    }
}

#[derive(Clone)]
pub(crate) struct TaskCgroupState {
    task_id: TaskId,
    directory: PathBuf,
    directory_identity: DirectoryIdentity,
    budget: CgroupResourceBudget,
}

impl TaskCgroupState {
    pub(crate) fn task_id(&self) -> TaskId {
        self.task_id
    }

    pub(crate) fn validate(&self) -> Result<(), ProcessAdapterError> {
        self.validate_identity()?;
        let memory_max = fs::read_to_string(self.directory.join("memory.max"))
            .map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
        let swap_max = fs::read_to_string(self.directory.join("memory.swap.max"))
            .map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
        let oom_group = fs::read_to_string(self.directory.join("memory.oom.group"))
            .map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
        if memory_max.trim() != self.budget.memory_bytes.to_string()
            || swap_max.trim() != "0"
            || oom_group.trim() != "1"
        {
            return Err(ProcessAdapterError::InvalidResourceControl);
        }
        Ok(())
    }

    fn validate_identity(&self) -> Result<(), ProcessAdapterError> {
        let identity = DirectoryIdentity::read(&self.directory)
            .map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
        if identity != self.directory_identity {
            return Err(ProcessAdapterError::InvalidResourceControl);
        }
        for file in [
            "cgroup.procs",
            "cgroup.kill",
            "cgroup.events",
            "cpu.stat",
            "memory.current",
            "memory.events",
            "memory.max",
            "memory.swap.max",
            "memory.oom.group",
        ] {
            if !self.directory.join(file).is_file() {
                return Err(ProcessAdapterError::InvalidResourceControl);
            }
        }
        Ok(())
    }

    pub(crate) fn launch_identity(&self) -> (PathBuf, u64, u64) {
        let (device, inode) = self.directory_identity.raw();
        (self.directory.join("cgroup.procs"), device, inode)
    }

    pub(crate) fn limit_reached(&self) -> Result<bool, ProcessAdapterError> {
        self.validate()?;
        let cpu_usage = read_keyed_u64(&self.directory.join("cpu.stat"), "usage_usec")?;
        let memory_max_events = read_keyed_u64(&self.directory.join("memory.events"), "max")?;
        let oom_kills = read_keyed_u64(&self.directory.join("memory.events"), "oom_kill")?;
        Ok(cpu_usage >= self.budget.cpu_time_micros || memory_max_events > 0 || oom_kills > 0)
    }

    pub(crate) fn terminate(&self) -> Result<(), ProcessAdapterError> {
        self.validate_identity()?;
        fs::write(self.directory.join("cgroup.kill"), "1")
            .map_err(|_| ProcessAdapterError::InvalidResourceControl)
    }
}

fn canonical_cgroup_root(path: &Path) -> Result<PathBuf, ProcessAdapterError> {
    if !path.is_absolute() || path.as_os_str().as_encoded_bytes().len() > MAX_SANDBOX_PATH_BYTES {
        return Err(ProcessAdapterError::InvalidResourceControl);
    }
    let metadata =
        fs::symlink_metadata(path).map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ProcessAdapterError::InvalidResourceControl);
    }
    let canonical =
        fs::canonicalize(path).map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
    let mount = Path::new(CGROUP2_MOUNT);
    if canonical == mount || !canonical.starts_with(mount) {
        return Err(ProcessAdapterError::InvalidResourceControl);
    }
    Ok(canonical)
}

fn validate_cgroup_root(path: &Path) -> Result<(), ProcessAdapterError> {
    for file in [
        "cgroup.controllers",
        "cgroup.procs",
        "cgroup.subtree_control",
    ] {
        if !path.join(file).is_file() {
            return Err(ProcessAdapterError::InvalidResourceControl);
        }
    }
    let controllers = fs::read_to_string(path.join("cgroup.controllers"))
        .map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
    let enabled = fs::read_to_string(path.join("cgroup.subtree_control"))
        .map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
    if !contains_word(&controllers, "cpu")
        || !contains_word(&controllers, "memory")
        || !contains_word(&enabled, "cpu")
        || !contains_word(&enabled, "memory")
    {
        return Err(ProcessAdapterError::InvalidResourceControl);
    }
    let root_processes = fs::read_to_string(path.join("cgroup.procs"))
        .map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
    if !root_processes.trim().is_empty() {
        return Err(ProcessAdapterError::InvalidResourceControl);
    }

    let current = current_cgroup_directory()?;
    if current == path || !current.starts_with(path) {
        return Err(ProcessAdapterError::InvalidResourceControl);
    }
    Ok(())
}

fn current_cgroup_directory() -> Result<PathBuf, ProcessAdapterError> {
    let membership = fs::read_to_string("/proc/self/cgroup")
        .map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
    let relative = membership
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .ok_or(ProcessAdapterError::InvalidResourceControl)?;
    let relative = relative
        .strip_prefix('/')
        .ok_or(ProcessAdapterError::InvalidResourceControl)?;
    fs::canonicalize(Path::new(CGROUP2_MOUNT).join(relative))
        .map_err(|_| ProcessAdapterError::InvalidResourceControl)
}

fn configure_task_cgroup(
    directory: &Path,
    budget: CgroupResourceBudget,
) -> Result<(), ProcessAdapterError> {
    for file in [
        "cgroup.procs",
        "cgroup.kill",
        "cgroup.events",
        "cpu.stat",
        "memory.current",
        "memory.events",
        "memory.max",
        "memory.swap.max",
        "memory.oom.group",
    ] {
        if !directory.join(file).is_file() {
            return Err(ProcessAdapterError::InvalidResourceControl);
        }
    }
    fs::write(
        directory.join("memory.max"),
        budget.memory_bytes.to_string(),
    )
    .and_then(|()| fs::write(directory.join("memory.swap.max"), "0"))
    .and_then(|()| fs::write(directory.join("memory.oom.group"), "1"))
    .map_err(|_| ProcessAdapterError::InvalidResourceControl)
}

fn read_keyed_u64(path: &Path, key: &str) -> Result<u64, ProcessAdapterError> {
    let contents =
        fs::read_to_string(path).map_err(|_| ProcessAdapterError::InvalidResourceControl)?;
    contents
        .lines()
        .find_map(|line| {
            let mut fields = line.split_ascii_whitespace();
            let name = fields.next()?;
            let value = fields.next()?;
            (name == key && fields.next().is_none()).then_some(value)
        })
        .ok_or(ProcessAdapterError::InvalidResourceControl)?
        .parse()
        .map_err(|_| ProcessAdapterError::InvalidResourceControl)
}

fn wait_until_empty(state: &TaskCgroupState) -> Result<(), ProcessAdapterError> {
    let deadline = Instant::now() + CLEANUP_TIMEOUT;
    loop {
        state.validate_identity()?;
        let populated = read_keyed_u64(&state.directory.join("cgroup.events"), "populated")?;
        if populated == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(ProcessAdapterError::InvalidResourceControl);
        }
        thread::sleep(CLEANUP_POLL_INTERVAL);
    }
}

fn contains_word(contents: &str, expected: &str) -> bool {
    contents
        .split_ascii_whitespace()
        .any(|word| word == expected)
}

#[cfg(test)]
mod tests {
    use super::CgroupResourceBudget;
    use crate::ProcessAdapterError;
    use std::time::Duration;

    #[test]
    fn validates_resource_budget_bounds() {
        assert!(CgroupResourceBudget::new(Duration::from_secs(1), 1).is_ok());
        assert!(matches!(
            CgroupResourceBudget::new(Duration::ZERO, 1),
            Err(ProcessAdapterError::InvalidConfig)
        ));
        assert!(matches!(
            CgroupResourceBudget::new(Duration::from_secs(1), 0),
            Err(ProcessAdapterError::InvalidConfig)
        ));
        assert!(matches!(
            CgroupResourceBudget::new(Duration::from_secs(3_601), 1),
            Err(ProcessAdapterError::InvalidConfig)
        ));
    }
}
