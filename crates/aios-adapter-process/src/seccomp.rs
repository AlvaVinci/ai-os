use super::ProcessAdapterError;

#[cfg(any(target_os = "linux", test))]
const AUDIT_ARCH_X86_64: u32 = 0xc000_003e;
#[cfg(any(target_os = "linux", test))]
const X32_SYSCALL_BIT: u32 = 0x4000_0000;
#[cfg(any(target_os = "linux", test))]
const MAX_REVIEWED_SYSCALL: u32 = 470;

#[cfg(any(target_os = "linux", test))]
const BPF_LD_W_ABS: u16 = 0x20;
#[cfg(any(target_os = "linux", test))]
const BPF_JMP_JEQ_K: u16 = 0x15;
#[cfg(any(target_os = "linux", test))]
const BPF_JMP_JGE_K: u16 = 0x35;
#[cfg(any(target_os = "linux", test))]
const BPF_JMP_JGT_K: u16 = 0x25;
#[cfg(any(target_os = "linux", test))]
const BPF_RET_K: u16 = 0x06;

#[cfg(any(target_os = "linux", test))]
const SECCOMP_DATA_NR_OFFSET: u32 = 0;
#[cfg(any(target_os = "linux", test))]
const SECCOMP_DATA_ARCH_OFFSET: u32 = 4;
#[cfg(any(target_os = "linux", test))]
const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
#[cfg(any(target_os = "linux", test))]
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;

// Linux x86_64 syscall numbers. The v0.1 release target is one Linux x86_64 device.
#[cfg(any(target_os = "linux", test))]
const BLOCKED_SYSCALLS: &[(&str, u32)] = &[
    ("ptrace", 101),
    ("syslog", 103),
    ("personality", 135),
    ("vhangup", 153),
    ("pivot_root", 155),
    ("acct", 163),
    ("settimeofday", 164),
    ("mount", 165),
    ("umount2", 166),
    ("swapon", 167),
    ("swapoff", 168),
    ("reboot", 169),
    ("iopl", 172),
    ("ioperm", 173),
    ("init_module", 175),
    ("delete_module", 176),
    ("quotactl", 179),
    ("lookup_dcookie", 212),
    ("clock_settime", 227),
    ("kexec_load", 246),
    ("add_key", 248),
    ("request_key", 249),
    ("keyctl", 250),
    ("unshare", 272),
    ("perf_event_open", 298),
    ("fanotify_init", 300),
    ("name_to_handle_at", 303),
    ("open_by_handle_at", 304),
    ("clock_adjtime", 305),
    ("setns", 308),
    ("process_vm_readv", 310),
    ("process_vm_writev", 311),
    ("kcmp", 312),
    ("finit_module", 313),
    ("kexec_file_load", 320),
    ("bpf", 321),
    ("userfaultfd", 323),
    ("io_uring_setup", 425),
    ("io_uring_enter", 426),
    ("io_uring_register", 427),
    ("open_tree", 428),
    ("move_mount", 429),
    ("fsopen", 430),
    ("fsconfig", 431),
    ("fsmount", 432),
    ("fspick", 433),
    ("pidfd_getfd", 438),
    ("process_madvise", 440),
    ("mount_setattr", 442),
    ("quotactl_fd", 443),
    ("memfd_secret", 447),
    ("process_mrelease", 448),
    ("statmount", 457),
    ("listmount", 458),
    ("lsm_set_self_attr", 460),
    ("lsm_list_modules", 461),
    ("open_tree_attr", 467),
    ("file_getattr", 468),
    ("file_setattr", 469),
    ("listns", 470),
];

#[cfg(any(target_os = "linux", test))]
#[derive(Clone, Copy)]
struct FilterInstruction {
    code: u16,
    jump_true: u8,
    jump_false: u8,
    value: u32,
}

#[cfg(any(target_os = "linux", test))]
impl FilterInstruction {
    const fn statement(code: u16, value: u32) -> Self {
        Self {
            code,
            jump_true: 0,
            jump_false: 0,
            value,
        }
    }

    const fn jump(code: u16, value: u32, jump_true: u8, jump_false: u8) -> Self {
        Self {
            code,
            jump_true,
            jump_false,
            value,
        }
    }

    fn append_bytes(self, output: &mut Vec<u8>) {
        output.extend_from_slice(&self.code.to_ne_bytes());
        output.push(self.jump_true);
        output.push(self.jump_false);
        output.extend_from_slice(&self.value.to_ne_bytes());
    }
}

pub(crate) fn ensure_supported_target() -> Result<(), ProcessAdapterError> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(())
    } else {
        Err(ProcessAdapterError::UnsupportedPlatform)
    }
}

#[cfg(any(target_os = "linux", test))]
fn default_filter_bytes() -> Vec<u8> {
    let mut instructions = Vec::with_capacity(9 + BLOCKED_SYSCALLS.len() * 2);
    instructions.push(FilterInstruction::statement(
        BPF_LD_W_ABS,
        SECCOMP_DATA_ARCH_OFFSET,
    ));
    instructions.push(FilterInstruction::jump(
        BPF_JMP_JEQ_K,
        AUDIT_ARCH_X86_64,
        1,
        0,
    ));
    instructions.push(FilterInstruction::statement(
        BPF_RET_K,
        SECCOMP_RET_KILL_PROCESS,
    ));
    instructions.push(FilterInstruction::statement(
        BPF_LD_W_ABS,
        SECCOMP_DATA_NR_OFFSET,
    ));
    instructions.push(FilterInstruction::jump(
        BPF_JMP_JGE_K,
        X32_SYSCALL_BIT,
        0,
        1,
    ));
    instructions.push(FilterInstruction::statement(
        BPF_RET_K,
        SECCOMP_RET_KILL_PROCESS,
    ));
    instructions.push(FilterInstruction::jump(
        BPF_JMP_JGT_K,
        MAX_REVIEWED_SYSCALL,
        0,
        1,
    ));
    instructions.push(FilterInstruction::statement(
        BPF_RET_K,
        SECCOMP_RET_KILL_PROCESS,
    ));
    for (_, syscall) in BLOCKED_SYSCALLS {
        instructions.push(FilterInstruction::jump(BPF_JMP_JEQ_K, *syscall, 0, 1));
        instructions.push(FilterInstruction::statement(
            BPF_RET_K,
            SECCOMP_RET_KILL_PROCESS,
        ));
    }
    instructions.push(FilterInstruction::statement(BPF_RET_K, SECCOMP_RET_ALLOW));

    let mut bytes = Vec::with_capacity(instructions.len() * 8);
    for instruction in instructions {
        instruction.append_bytes(&mut bytes);
    }
    bytes
}

#[cfg(target_os = "linux")]
pub(crate) struct SeccompFilterFile {
    file: std::fs::File,
}

#[cfg(target_os = "linux")]
impl SeccompFilterFile {
    pub(crate) fn create() -> Result<Self, ProcessAdapterError> {
        use std::io::{Seek, SeekFrom, Write};
        use std::os::fd::AsRawFd;

        use rustix::fs::{MemfdFlags, SealFlags, fcntl_add_seals, memfd_create};
        use rustix::io::{FdFlags, fcntl_setfd};

        ensure_supported_target()?;
        let descriptor = memfd_create(
            "aios-seccomp",
            MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING,
        )
        .map_err(|_| ProcessAdapterError::SeccompUnavailable)?;
        let mut file = std::fs::File::from(descriptor);
        file.write_all(&default_filter_bytes())
            .and_then(|()| file.seek(SeekFrom::Start(0)).map(|_| ()))
            .map_err(|_| ProcessAdapterError::SeccompUnavailable)?;
        fcntl_add_seals(
            &file,
            SealFlags::WRITE | SealFlags::GROW | SealFlags::SHRINK | SealFlags::SEAL,
        )
        .map_err(|_| ProcessAdapterError::SeccompUnavailable)?;
        fcntl_setfd(&file, FdFlags::empty())
            .map_err(|_| ProcessAdapterError::SeccompUnavailable)?;

        if file.as_raw_fd() <= 2 {
            return Err(ProcessAdapterError::SeccompUnavailable);
        }
        Ok(Self { file })
    }

    pub(crate) fn raw_fd(&self) -> i32 {
        use std::os::fd::AsRawFd;

        self.file.as_raw_fd()
    }
}

#[cfg(not(target_os = "linux"))]
pub(crate) struct SeccompFilterFile;

#[cfg(not(target_os = "linux"))]
impl SeccompFilterFile {
    pub(crate) fn create() -> Result<Self, ProcessAdapterError> {
        Err(ProcessAdapterError::UnsupportedPlatform)
    }

    pub(crate) fn raw_fd(&self) -> i32 {
        -1
    }
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::SeccompFilterFile;
    use super::{
        AUDIT_ARCH_X86_64, BLOCKED_SYSCALLS, BPF_JMP_JEQ_K, BPF_JMP_JGE_K, BPF_JMP_JGT_K,
        BPF_LD_W_ABS, BPF_RET_K, MAX_REVIEWED_SYSCALL, SECCOMP_DATA_ARCH_OFFSET,
        SECCOMP_DATA_NR_OFFSET, SECCOMP_RET_ALLOW, SECCOMP_RET_KILL_PROCESS, X32_SYSCALL_BIT,
        default_filter_bytes,
    };

    fn instruction(bytes: &[u8], index: usize) -> (u16, u8, u8, u32) {
        let offset = index * 8;
        (
            u16::from_ne_bytes([bytes[offset], bytes[offset + 1]]),
            bytes[offset + 2],
            bytes[offset + 3],
            u32::from_ne_bytes([
                bytes[offset + 4],
                bytes[offset + 5],
                bytes[offset + 6],
                bytes[offset + 7],
            ]),
        )
    }

    fn evaluate(bytes: &[u8], architecture: u32, syscall: u32) -> u32 {
        let mut accumulator = 0_u32;
        let mut program_counter = 0_usize;
        loop {
            let (code, jump_true, jump_false, value) = instruction(bytes, program_counter);
            match code {
                BPF_LD_W_ABS => {
                    accumulator = match value {
                        SECCOMP_DATA_NR_OFFSET => syscall,
                        SECCOMP_DATA_ARCH_OFFSET => architecture,
                        _ => panic!("unexpected seccomp data offset"),
                    };
                    program_counter += 1;
                }
                BPF_JMP_JEQ_K => {
                    let jump = if accumulator == value {
                        jump_true
                    } else {
                        jump_false
                    };
                    program_counter += usize::from(jump) + 1;
                }
                BPF_JMP_JGE_K => {
                    let jump = if accumulator >= value {
                        jump_true
                    } else {
                        jump_false
                    };
                    program_counter += usize::from(jump) + 1;
                }
                BPF_JMP_JGT_K => {
                    let jump = if accumulator > value {
                        jump_true
                    } else {
                        jump_false
                    };
                    program_counter += usize::from(jump) + 1;
                }
                BPF_RET_K => return value,
                _ => panic!("unexpected BPF instruction"),
            }
        }
    }

    #[test]
    fn policy_validates_architecture_and_rejects_x32_before_syscalls() {
        let bytes = default_filter_bytes();
        assert_eq!(bytes.len() % 8, 0);
        assert_eq!(
            instruction(&bytes, 0),
            (BPF_LD_W_ABS, 0, 0, SECCOMP_DATA_ARCH_OFFSET)
        );
        assert_eq!(
            instruction(&bytes, 1),
            (BPF_JMP_JEQ_K, 1, 0, AUDIT_ARCH_X86_64)
        );
        assert_eq!(
            instruction(&bytes, 2),
            (BPF_RET_K, 0, 0, SECCOMP_RET_KILL_PROCESS)
        );
        assert_eq!(
            instruction(&bytes, 3),
            (BPF_LD_W_ABS, 0, 0, SECCOMP_DATA_NR_OFFSET)
        );
        assert_eq!(
            instruction(&bytes, 4),
            (BPF_JMP_JGE_K, 0, 1, X32_SYSCALL_BIT)
        );
        assert_eq!(
            instruction(&bytes, 5),
            (BPF_RET_K, 0, 0, SECCOMP_RET_KILL_PROCESS)
        );
        assert_eq!(
            instruction(&bytes, 6),
            (BPF_JMP_JGT_K, 0, 1, MAX_REVIEWED_SYSCALL)
        );
        assert_eq!(
            instruction(&bytes, 7),
            (BPF_RET_K, 0, 0, SECCOMP_RET_KILL_PROCESS)
        );
    }

    #[test]
    fn policy_blocks_a_sorted_unique_reviewed_syscall_set_then_allows() {
        for pair in BLOCKED_SYSCALLS.windows(2) {
            assert!(pair[0].1 < pair[1].1, "syscall list must be sorted");
        }

        let bytes = default_filter_bytes();
        for (index, (_, syscall)) in BLOCKED_SYSCALLS.iter().enumerate() {
            assert_eq!(
                instruction(&bytes, 8 + index * 2),
                (BPF_JMP_JEQ_K, 0, 1, *syscall)
            );
            assert_eq!(
                instruction(&bytes, 9 + index * 2),
                (BPF_RET_K, 0, 0, SECCOMP_RET_KILL_PROCESS)
            );
        }
        assert_eq!(
            instruction(&bytes, 8 + BLOCKED_SYSCALLS.len() * 2),
            (BPF_RET_K, 0, 0, SECCOMP_RET_ALLOW)
        );

        assert_eq!(
            evaluate(&bytes, 0, 39),
            SECCOMP_RET_KILL_PROCESS,
            "unknown architecture must fail closed"
        );
        assert_eq!(
            evaluate(&bytes, AUDIT_ARCH_X86_64, X32_SYSCALL_BIT),
            SECCOMP_RET_KILL_PROCESS,
            "x32 ABI must not bypass the deny table"
        );
        assert_eq!(
            evaluate(&bytes, AUDIT_ARCH_X86_64, MAX_REVIEWED_SYSCALL + 1),
            SECCOMP_RET_KILL_PROCESS,
            "future syscall numbers require an explicit policy review"
        );
        assert_eq!(
            evaluate(&bytes, AUDIT_ARCH_X86_64, 135),
            SECCOMP_RET_KILL_PROCESS,
            "personality is part of the reviewed deny table"
        );
        assert_eq!(
            evaluate(&bytes, AUDIT_ARCH_X86_64, 39),
            SECCOMP_RET_ALLOW,
            "ordinary x86_64 getpid remains allowed"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn policy_descriptor_is_fully_sealed_and_explicitly_inheritable() {
        use rustix::fs::{SealFlags, fcntl_get_seals};
        use rustix::io::{FdFlags, fcntl_getfd};

        let filter = SeccompFilterFile::create().expect("create sealed seccomp filter");
        let seals = fcntl_get_seals(&filter.file).expect("read seccomp descriptor seals");
        assert!(seals.contains(SealFlags::WRITE));
        assert!(seals.contains(SealFlags::GROW));
        assert!(seals.contains(SealFlags::SHRINK));
        assert!(seals.contains(SealFlags::SEAL));

        let descriptor_flags =
            fcntl_getfd(&filter.file).expect("read seccomp descriptor inheritance flags");
        assert!(!descriptor_flags.contains(FdFlags::CLOEXEC));
    }
}
