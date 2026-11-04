// SPDX-FileCopyrightText: 2026  John Hamilton
// SPDX-License-Identifier: GPL-3.0-or-later

//! The sandbox of the child that reads a file with GStreamer
//! (`audio::track::discovered`).
//!
//! GStreamer's demuxers and parsers are written in C, and the files they read
//! can be crafted, so a file that exploits a bug in one of them runs its code
//! in the child. Reading each file in a child already keeps a crash to that
//! file; the sandbox keeps an exploit there too. The child can read the
//! system's files, GStreamer's registry and the one file it was started for,
//! and write to the pipes it was given, and nothing else: it can't open a file
//! for writing, read the user's files, reach the network, the session bus, the
//! display or the sound server, start a program, or signal or trace another
//! process. Its only way out is the report the parent reads, which has a size
//! limit and is parsed into tags.
//!
//! The child confines itself, rather than being started in bwrap: bwrap can't
//! run inside the Flatpak, and needs user namespaces that some systems don't
//! allow, while Landlock and seccomp work the same in the Flatpak, on the host
//! and in containers, without privileges. The layers, in the order they're
//! applied:
//!
//! - Descriptors other than stdin, stdout and stderr are closed, as a file
//!   opened before the restriction keeps the access it was opened with.
//! - Resource limits keep a crafted file from taking all the memory, or from
//!   leaving a core dump that holds its bytes.
//! - Landlock limits the files that can be opened, and how: reading only, the
//!   paths in `readable_paths`. It's best effort: a kernel without Landlock
//!   (before 5.13, or without `landlock` in its `lsm=` list) leaves every
//!   file the user can read readable, and the other layers still hold. That
//!   includes the memory of the user's other processes, in
//!   `/proc/<pid>/mem`, where Yama's `ptrace_scope` is 0: seccomp can't tell
//!   one path from another. An older Landlock enforces the rights it knows.
//! - seccomp limits the system calls to those that reading the file takes,
//!   and it's required: a child that can't set it up leaves its file unread.
//!
//! All of it happens before GStreamer starts, as Landlock applies to the
//! thread that restricts itself and to the threads it starts afterwards, and
//! GStreamer starts its threads as it initializes.
//!
//! A system call that isn't allowed fails with ENOSYS, as if the kernel didn't
//! have it, which glibc and GLib fall back from (`clone3` to `clone`, `openat2`
//! to `openat`). So when a newer glibc or GStreamer needs a call that isn't
//! allowed, or a file that isn't granted, reading degrades rather than every
//! file being left unread. What's missing shows as ENOSYS and EACCES results
//! when the child is run under `strace -f`, as
//! `dismas --discover-file covers <file>`.

use std::path::Path;

// The exit code of a child that couldn't set up its sandbox, and so left its
// file unread; it writes why on its stdout, in place of the report
pub(crate) const EXIT_NO_SANDBOX: u8 = 3;

// How confined the child is
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(
    not(target_os = "linux"),
    allow(dead_code, reason = "only Linux has a sandbox")
)]
pub(crate) enum Status {
    // Landlock with every right it was asked for, and seccomp
    Confined,
    // Landlock without some of the rights it was asked for, as the kernel's
    // is older, and seccomp; the files are confined all the same
    PartlyConfined,
    // seccomp alone, as the kernel has no Landlock: files the user can read
    // can still be read
    SeccompOnly,
    // Neither, outside Linux
    #[cfg(not(target_os = "linux"))]
    Unconfined,
}

impl Status {
    // Whether only the granted files can be read
    #[cfg(test)]
    pub(crate) fn confines_files(self) -> bool {
        matches!(self, Status::Confined | Status::PartlyConfined)
    }
}

impl std::fmt::Display for Status {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Status::Confined => "Landlock and seccomp",
            Status::PartlyConfined => "the kernel's older Landlock and seccomp",
            Status::SeccompOnly => "seccomp, as the kernel has no Landlock",
            #[cfg(not(target_os = "linux"))]
            Status::Unconfined => "no sandbox, outside Linux",
        })
    }
}

// Outside Linux, the child reads its file as it is, and the parent says so
// once
#[cfg(not(target_os = "linux"))]
pub(crate) fn restrict_to_reading(_file: &Path) -> Result<Status, String> {
    Ok(Status::Unconfined)
}

// Confines the child to reading `file` and the files GStreamer needs, before
// it starts GStreamer; an error says why it couldn't
#[cfg(target_os = "linux")]
pub(crate) fn restrict_to_reading(file: &Path) -> Result<Status, String> {
    linux::close_other_descriptors()?;
    linux::limit_resources()?;
    let landlock = linux::restrict_files(file)?;
    linux::filter_system_calls()
        .and_then(|filter| filter.load())
        .map_err(|e| format!("Unable to filter the system calls: {e}"))?;
    Ok(match landlock {
        landlock::RulesetStatus::FullyEnforced => Status::Confined,
        landlock::RulesetStatus::PartiallyEnforced => Status::PartlyConfined,
        landlock::RulesetStatus::NotEnforced => Status::SeccompOnly,
    })
}

// The files and folders the child can read, besides its file: the system's,
// the user's GStreamer registry and plugins, and those GStreamer's variables
// and the library path name. `var` looks up a variable, and `cache` and
// `data` are the user's cache and data folders
#[cfg(target_os = "linux")]
fn readable_paths(
    var: impl Fn(&str) -> Option<std::ffi::OsString>,
    cache: &Path,
    data: &Path,
) -> Vec<std::path::PathBuf> {
    // The libraries, GStreamer's plugins and their data; the Flatpak's
    // prefix; those of NixOS and Guix; ld.so's cache, which it reads again as
    // GStreamer loads a plugin, the time zone, and what glibc reads to look
    // up the user; the devices GLib reads random bytes from; and what glibc
    // and ORC read to count the processors and find their features
    const SYSTEM: &[&str] = &[
        "/usr",
        "/lib",
        "/lib32",
        "/lib64",
        "/app",
        "/nix/store",
        "/gnu/store",
        "/etc",
        "/dev/null",
        "/dev/random",
        "/dev/urandom",
        "/proc/cpuinfo",
        "/sys/devices/system/cpu",
    ];
    let mut paths: Vec<_> = SYSTEM.iter().map(std::path::PathBuf::from).collect();
    // GStreamer's registry, where it keeps what each plugin has, and the
    // plugins the user installed
    paths.push(cache.join("gstreamer-1.0"));
    paths.push(data.join("gstreamer-1.0").join("plugins"));
    // A registry elsewhere: the file alone, as its folder could be any
    paths.extend(
        ["GST_REGISTRY_1_0", "GST_REGISTRY"]
            .into_iter()
            .filter_map(|name| var(name).map(Into::into)),
    );
    // The folders of plugins and libraries in the variables; an empty entry,
    // which stands for the current folder, can't be opened, and is skipped
    for name in [
        "GST_PLUGIN_PATH_1_0",
        "GST_PLUGIN_PATH",
        "GST_PLUGIN_SYSTEM_PATH_1_0",
        "GST_PLUGIN_SYSTEM_PATH",
        "LD_LIBRARY_PATH",
    ] {
        if let Some(value) = var(name) {
            paths.extend(std::env::split_paths(&value));
        }
    }
    paths
}

#[cfg(target_os = "linux")]
mod linux {
    use std::{fs::OpenOptions, os::unix::fs::OpenOptionsExt, path::Path};

    use landlock::{
        ABI, Access, AccessFs, AccessNet, PathBeneath, Ruleset, RulesetAttr, RulesetCreatedAttr,
        RulesetError, RulesetStatus, Scope, path_beneath_rules,
    };
    use libseccomp::{
        ScmpAction, ScmpArgCompare, ScmpCompareOp, ScmpFilterContext, ScmpSyscall,
        error::SeccompError,
    };
    use log::debug;

    // The Landlock rights handled: those of the newest version the crate
    // knows, 9, whose kernel denies them all but those granted; an older
    // kernel handles those it knows. Before raising it, look at what the new
    // version adds: a right the child takes and isn't granted would be denied
    const LANDLOCK_ABI: ABI = ABI::V9;

    // The most memory the child takes for its data, its heap and its
    // threads' stacks: Discoverer takes tens of MB, and the video decoder of
    // a WebM a few hundred as it decodes the first frames of a 4K video
    const MAX_DATA: libc::rlim_t = 1 << 30;
    // The most address space, for the memory that the data limit doesn't
    // count, like shared mappings; the libraries and glibc's arenas, which
    // reserve more than they use, take a few hundred MB more of it than of
    // memory
    const MAX_ADDRESS_SPACE: libc::rlim_t = 4 << 30;
    // The largest stack of a thread
    const MAX_STACK: libc::rlim_t = 8 << 20;

    // Closes the descriptors inherited by mistake, as a file opened before the
    // Landlock restriction keeps the access it was opened with, and seccomp
    // can't take reading or writing away from an open descriptor
    pub(super) fn close_other_descriptors() -> Result<(), String> {
        // SAFETY: nothing in the child uses a descriptor above stderr yet,
        // as GStreamer hasn't started; the call takes no pointers
        let closed = unsafe { libc::syscall(libc::SYS_close_range, 3, libc::c_uint::MAX, 0) };
        let error = std::io::Error::last_os_error();
        // Kernels before 5.9 don't have close_range
        if closed != 0 && error.raw_os_error() != Some(libc::ENOSYS) {
            return Err(format!("Unable to close the descriptors: {error}"));
        }
        Ok(())
    }

    unsafe extern "C" {
        // The attributes of the threads made without attributes of their
        // own, in glibc and musl
        fn pthread_getattr_default_np(attr: *mut libc::pthread_attr_t) -> libc::c_int;
        fn pthread_setattr_default_np(attr: *const libc::pthread_attr_t) -> libc::c_int;
    }

    // Limits the memory the child can take, and leaves no core dump of a
    // crash, which would hold the bytes of the crafted file. The hard limits
    // are lowered too, so that an exploit can't raise them again; limits that
    // are already lower stay
    pub(super) fn limit_resources() -> Result<(), String> {
        let lower = |resource, max: libc::rlim_t, name: &str| {
            let mut limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: 0,
            };
            // SAFETY: getrlimit and setrlimit only read and write `limit`
            let lowered = unsafe {
                libc::getrlimit(resource, &mut limit) == 0 && {
                    limit.rlim_cur = limit.rlim_cur.min(max);
                    limit.rlim_max = limit.rlim_max.min(max);
                    libc::setrlimit(resource, &limit) == 0
                }
            };
            if lowered {
                Ok(())
            } else {
                Err(format!(
                    "Unable to limit the {name}: {}",
                    std::io::Error::last_os_error()
                ))
            }
        };
        lower(libc::RLIMIT_DATA, MAX_DATA, "memory")?;
        lower(libc::RLIMIT_AS, MAX_ADDRESS_SPACE, "address space")?;
        // The main thread's
        lower(libc::RLIMIT_STACK, MAX_STACK, "stack")?;
        lower(libc::RLIMIT_CORE, 0, "core dumps")?;
        limit_thread_stacks()
    }

    // The threads that GLib and GStreamer start get the default stack size,
    // which glibc takes from the stack limit as the program starts, so
    // lowering the limit doesn't change it: a `ulimit -s` of a gigabyte would
    // give each thread a gigabyte of stack, which the data limit counts, and
    // the first thread couldn't start
    fn limit_thread_stacks() -> Result<(), String> {
        let mut attr = std::mem::MaybeUninit::<libc::pthread_attr_t>::uninit();
        // SAFETY: pthread_getattr_default_np initializes `attr`, which is
        // only used after it did, and destroyed at the end
        let code = unsafe {
            let mut code = pthread_getattr_default_np(attr.as_mut_ptr());
            if code == 0 {
                let mut size = 0;
                code = libc::pthread_attr_getstacksize(attr.as_ptr(), &mut size);
                if code == 0 && size as libc::rlim_t > MAX_STACK {
                    code = libc::pthread_attr_setstacksize(attr.as_mut_ptr(), MAX_STACK as usize);
                    if code == 0 {
                        code = pthread_setattr_default_np(attr.as_ptr());
                    }
                }
                libc::pthread_attr_destroy(attr.as_mut_ptr());
            }
            code
        };
        if code == 0 {
            Ok(())
        } else {
            Err(format!(
                "Unable to limit the stacks of threads: {}",
                std::io::Error::from_raw_os_error(code)
            ))
        }
    }

    // Lets the child open only the system's files, GStreamer's and its own
    // file, and only to read them, and keeps it from reaching TCP, sockets
    // in the abstract namespace (where the session bus can be), named sockets
    // and other processes' signals
    pub(super) fn restrict_files(file: &Path) -> Result<RulesetStatus, String> {
        let read = AccessFs::ReadFile | AccessFs::ReadDir;
        let paths = super::readable_paths(
            |name| std::env::var_os(name),
            &gtk::glib::user_cache_dir(),
            &gtk::glib::user_data_dir(),
        );
        // The file, opened without being read, which doesn't wait on a named
        // pipe; the rule is tied to what was opened, so a file put in its
        // place afterwards can't be read. A path that isn't a regular file
        // gets no rule, as a rule on a folder would let everything in it be
        // read, and one that can't be opened none either: GStreamer then
        // finds nothing, as it couldn't have read it anyway
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_PATH)
            .open(file)
            .ok()
            .filter(|file| file.metadata().is_ok_and(|m| m.is_file()));
        let restrict = || -> Result<RulesetStatus, RulesetError> {
            let mut ruleset = Ruleset::default()
                .handle_access(AccessFs::from_all(LANDLOCK_ABI))?
                .handle_access(AccessNet::from_all(LANDLOCK_ABI))?
                .scope(Scope::AbstractUnixSocket | Scope::Signal)?
                .create()?
                .add_rules(path_beneath_rules(paths, read))?;
            if let Some(file) = file {
                ruleset = ruleset.add_rule(PathBeneath::new(file, AccessFs::ReadFile))?;
            }
            Ok(ruleset.restrict_self()?.ruleset)
        };
        restrict().map_err(|e| format!("Unable to restrict the files: {e}"))
    }

    // The system calls allowed whatever their arguments, those that the
    // reading takes, as found by tracing the child on Ogg FLAC and WebM files,
    // and those that glibc and GLib take on paths that a short file doesn't:
    // reading and looking at files, memory, threads, signals to the child
    // itself, time, waiting. Writing only goes to the descriptors the child
    // has, stdout and stderr; opening is allowed below, for reading only
    const ALLOWED: &[&str] = &[
        // Files
        "read",
        "readv",
        "pread64",
        "preadv",
        "write",
        "writev",
        "close",
        "lseek",
        "fadvise64",
        "fstat",
        "stat",
        "lstat",
        "newfstatat",
        "statx",
        "statfs",
        "fstatfs",
        "getdents64",
        "readlink",
        "readlinkat",
        "access",
        "faccessat",
        "faccessat2",
        "getcwd",
        "fcntl",
        "dup",
        "dup2",
        "dup3",
        "pipe",
        "pipe2",
        "eventfd2",
        // Waiting on descriptors
        "poll",
        "ppoll",
        "select",
        "pselect6",
        "epoll_create1",
        "epoll_ctl",
        "epoll_wait",
        "epoll_pwait",
        "epoll_pwait2",
        // Memory
        "brk",
        "mmap",
        "munmap",
        "mprotect",
        "mremap",
        "madvise",
        "membarrier",
        // Threads
        "futex",
        "futex_waitv",
        "set_robust_list",
        "rseq",
        "sched_yield",
        "sched_getaffinity",
        "gettid",
        "getpid",
        "exit",
        "exit_group",
        // Signals
        "rt_sigaction",
        "rt_sigprocmask",
        "rt_sigreturn",
        "sigaltstack",
        "restart_syscall",
        // Time
        "clock_gettime",
        "clock_getres",
        "clock_nanosleep",
        "gettimeofday",
        "nanosleep",
        "time",
        // The system and the user
        "uname",
        "sysinfo",
        "getrandom",
        "getrlimit",
        "getuid",
        "geteuid",
        "getgid",
        "getegid",
    ];

    // The system calls that end the child, rather than failing: those an
    // exploit would make to run a program or reach another process, which
    // the reading never does, so that an attempt shows in the parent's log as
    // a child ended by SIGSYS. execve and execveat also keep a program in a
    // memfd from being run, which Landlock doesn't see, as a memfd has no
    // path. The others fail with ENOSYS: socket, for one, can be tried by
    // glibc as it looks up a user, which then reads /etc/passwd
    const FATAL: &[&str] = &[
        "execve",
        "execveat",
        "ptrace",
        "process_vm_readv",
        "process_vm_writev",
    ];

    // The filter, which the tests build without loading it
    pub(super) fn filter_system_calls() -> Result<ScmpFilterContext, SeccompError> {
        use ScmpCompareOp::{Equal, MaskedEqual};

        // What isn't allowed fails as if the kernel didn't have it. A rule
        // can't have the filter's default action, so nothing is denied with
        // ENOSYS by a rule of its own: what's left out is
        let mut filter = ScmpFilterContext::new(ScmpAction::Errno(libc::ENOSYS))?;
        // A call made through another architecture's interface, like the
        // 32-bit one on x86_64, ends the process, rather than one thread,
        // after which the child would wait until its alarm
        filter.set_act_badarch(ScmpAction::KillProcess)?;
        // The filter applies to every thread, those of the tests too
        filter.set_ctl_tsync(true)?;

        let rule = |filter: &mut ScmpFilterContext,
                    action,
                    name: &str,
                    comparisons: &[ScmpArgCompare]|
         -> Result<(), SeccompError> {
            match ScmpSyscall::from_name(name) {
                Ok(syscall) => {
                    filter.add_rule_conditional(action, syscall, comparisons)?;
                }
                // An older libseccomp than the list was made with: a call it
                // doesn't know then fails as if the kernel didn't have it,
                // which glibc and GLib take for a kernel without it
                Err(_) => debug!("libseccomp doesn't know the system call {name}"),
            }
            Ok(())
        };
        for name in ALLOWED {
            rule(&mut filter, ScmpAction::Allow, name, &[])?;
        }
        for name in FATAL {
            rule(&mut filter, ScmpAction::KillProcess, name, &[])?;
        }

        // Opening for reading only, which keeps files from being written,
        // made or truncated whether the kernel has Landlock or not. O_TMPFILE
        // needs a write mode too, so it isn't checked apart, which would be
        // wrong anyway, as it includes O_DIRECTORY, which opendir() uses.
        // openat2 isn't allowed: glibc falls back to openat without it
        let writing =
            (libc::O_WRONLY | libc::O_RDWR | libc::O_CREAT | libc::O_TRUNC | libc::O_APPEND) as u64;
        rule(
            &mut filter,
            ScmpAction::Allow,
            "openat",
            &[ScmpArgCompare::new(2, MaskedEqual(writing), 0)],
        )?;
        rule(
            &mut filter,
            ScmpAction::Allow,
            "open",
            &[ScmpArgCompare::new(1, MaskedEqual(writing), 0)],
        )?;

        // Threads, which glibc makes with clone, as clone3 isn't allowed: no
        // other process, as the child starts none, and no namespaces. The
        // flags are the first argument (the second on s390x, which isn't
        // handled). CLONE_NEWTIME is left out, as clone takes that bit for
        // the signal sent at the end
        let new_namespaces = libc::CLONE_NEWNS
            | libc::CLONE_NEWCGROUP
            | libc::CLONE_NEWUTS
            | libc::CLONE_NEWIPC
            | libc::CLONE_NEWUSER
            | libc::CLONE_NEWPID
            | libc::CLONE_NEWNET;
        let thread = libc::CLONE_THREAD as u64;
        rule(
            &mut filter,
            ScmpAction::Allow,
            "clone",
            &[ScmpArgCompare::new(
                0,
                MaskedEqual((libc::CLONE_THREAD | new_namespaces) as u64),
                thread,
            )],
        )?;

        // A pair of connected sockets, which each GstBus makes to wake up
        // whatever waits on it: without it, Discoverer can't start
        rule(
            &mut filter,
            ScmpAction::Allow,
            "socketpair",
            &[ScmpArgCompare::new(0, Equal, libc::AF_UNIX as u64)],
        )?;

        // Signals to the child's own threads only, as abort() and raise()
        // send
        let pid = u64::from(std::process::id());
        rule(
            &mut filter,
            ScmpAction::Allow,
            "tgkill",
            &[ScmpArgCompare::new(0, Equal, pid)],
        )?;

        // Whether a descriptor is a terminal, as the log asks of stderr; the
        // request is an int, whose upper bits can be anything
        #[allow(clippy::unnecessary_cast, reason = "TCGETS is an int in musl")]
        let tcgets = libc::TCGETS as u64;
        rule(
            &mut filter,
            ScmpAction::Allow,
            "ioctl",
            &[ScmpArgCompare::new(1, MaskedEqual(0xFFFF_FFFF), tcgets)],
        )?;

        // The names of threads
        rule(
            &mut filter,
            ScmpAction::Allow,
            "prctl",
            &[ScmpArgCompare::new(0, Equal, libc::PR_SET_NAME as u64)],
        )?;

        // Resource limits read, not set, as glibc's getrlimit() does with a
        // null new limit
        rule(
            &mut filter,
            ScmpAction::Allow,
            "prlimit64",
            &[ScmpArgCompare::new(2, Equal, 0)],
        )?;

        Ok(filter)
    }

    #[cfg(test)]
    pub(super) fn listed_system_calls() -> impl Iterator<Item = &'static str> {
        ALLOWED.iter().chain(FATAL).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn gstreamers_variables_name_more_paths_to_read() {
        let var = |name: &str| match name {
            "GST_PLUGIN_PATH_1_0" => Some("/opt/gst/plugins:/srv/plugins".into()),
            "GST_PLUGIN_SYSTEM_PATH" => Some("/app/lib/gstreamer-1.0".into()),
            "GST_REGISTRY" => Some("/var/cache/registry.bin".into()),
            "LD_LIBRARY_PATH" => Some(":/app/lib".into()),
            _ => None,
        };
        let paths = readable_paths(
            var,
            Path::new("/home/user/.cache"),
            Path::new("/home/user/.local/share"),
        );
        for path in [
            "/usr",
            "/etc",
            "/home/user/.cache/gstreamer-1.0",
            "/home/user/.local/share/gstreamer-1.0/plugins",
            "/opt/gst/plugins",
            "/srv/plugins",
            "/app/lib/gstreamer-1.0",
            "/var/cache/registry.bin",
            "/app/lib",
        ] {
            assert!(paths.contains(&path.into()), "{path}");
        }
        // Not the registry's folder, nor the home folder
        assert!(!paths.contains(&"/var/cache".into()));
        assert!(!paths.iter().any(|path| path == Path::new("/home/user")));
    }

    // The filter is built at each read: a rule that libseccomp turns down,
    // or a name it doesn't know, would leave every file unread
    #[cfg(target_os = "linux")]
    #[test]
    fn the_system_call_filter_builds() {
        for name in linux::listed_system_calls() {
            assert!(libseccomp::ScmpSyscall::from_name(name).is_ok(), "{name}");
        }
        linux::filter_system_calls().unwrap();
    }
}
