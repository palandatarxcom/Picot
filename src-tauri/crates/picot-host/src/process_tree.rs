// ABOUTME: Owns platform-specific process-tree membership and termination.
// ABOUTME: Provides exact-child identity guards with bounded graceful escalation.
use std::process::{Child, ExitStatus};
use std::time::Duration;

#[derive(Debug)]
pub(crate) struct ProcessTree {
    #[cfg(unix)]
    pgid: libc::pid_t,
    #[cfg(windows)]
    job: windows_job::JobHandle,
}

impl ProcessTree {
    pub(crate) fn attach(child: &mut Child) -> Result<Self, String> {
        #[cfg(unix)]
        {
            let pid = child.id() as libc::pid_t;
            // The child joins its own group in pre_exec while the parent
            // probes here: two racers, one group. Racing setpgid from the
            // parent too is the standard idiom — the losing call observes
            // EACCES (child already exec'd, group already moved) or EPERM,
            // and ESRCH means the child died outright, which terminate()
            // handles through try_wait/ESRCH. Probing without the parent-side
            // setpgid leaves a window where getpgid still sees the pre-group
            // pid or -1 and misreports a fresh child as an identity violation.
            unsafe {
                if libc::setpgid(pid, pid) != 0 {
                    match std::io::Error::last_os_error().raw_os_error() {
                        Some(libc::EACCES) | Some(libc::EPERM) | Some(libc::ESRCH) => {}
                        Some(code) => {
                            return Err(format!(
                                "Cannot establish Pi child process group: errno {code}"
                            ));
                        }
                        None => {
                            return Err(
                                "Cannot establish Pi child process group: unknown errno".into()
                            );
                        }
                    }
                }
            }
            // A dead child probes as -1; that is not an identity violation.
            let group = unsafe { libc::getpgid(pid) };
            if group != pid && group != -1 {
                return Err(format!(
                    "Pi child process group identity mismatch: {group} != {pid}"
                ));
            }
            Ok(Self { pgid: pid })
        }
        #[cfg(windows)]
        {
            windows_job::create_and_assign(child.id())
                .map(|job| Self { job })
                .ok_or_else(|| "Cannot assign Pi process to Windows Job Object".into())
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = child;
            Ok(Self {})
        }
    }

    pub(crate) fn terminate(&mut self, child: &mut Child) -> Result<bool, String> {
        #[cfg(unix)]
        {
            let pid = child.id() as libc::pid_t;
            let child_exited = child.try_wait().map_err(|e| e.to_string())?.is_some();
            if !child_exited && unsafe { libc::getpgid(pid) } != self.pgid {
                return Err("Pi child process group identity changed before termination".into());
            }
            if unsafe { libc::kill(-self.pgid, libc::SIGTERM) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(false);
                }
                return Err(error.to_string());
            }
            if !child_exited {
                for _ in 0..20 {
                    if child.try_wait().map_err(|e| e.to_string())?.is_some() {
                        return Ok(true);
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                if unsafe { libc::kill(-self.pgid, libc::SIGKILL) } != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) {
                        return Err(error.to_string());
                    }
                }
                let _ = child.wait();
                return Ok(true);
            }
            if Self::process_group_alive(self.pgid)
                && unsafe { libc::kill(-self.pgid, libc::SIGKILL) } != 0
            {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(error.to_string());
                }
            }
            Ok(true)
        }
        #[cfg(windows)]
        {
            if let Err(job_error) = windows_job::terminate(&self.job) {
                child.kill().map_err(|child_error| {
                    format!("{job_error}; direct-child fallback failed: {child_error}")
                })?;
            }
            return Ok(true);
        }
        #[cfg(not(any(unix, windows)))]
        {
            child.kill().map_err(|e| e.to_string())?;
            return Ok(true);
        }
    }

    #[cfg(unix)]
    fn process_group_alive(pgid: libc::pid_t) -> bool {
        unsafe {
            libc::kill(-pgid, 0) == 0
                || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
        }
    }
    pub(crate) fn wait(child: &mut Child) -> Result<ExitStatus, String> {
        child.wait().map_err(|e| e.to_string())
    }
}

#[cfg(unix)]
pub(crate) fn configure_child(command: &mut std::process::Command) {
    use std::os::unix::process::CommandExt;
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
}
#[cfg(not(unix))]
pub(crate) fn configure_child(_command: &mut std::process::Command) {}

#[cfg(windows)]
mod windows_job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::*;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };
    #[derive(Debug)]
    pub struct JobHandle(HANDLE);
    unsafe impl Send for JobHandle {}
    unsafe impl Sync for JobHandle {}
    impl Drop for JobHandle {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    pub fn create_and_assign(pid: u32) -> Option<JobHandle> {
        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                return None;
            }
            let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                (&mut limits as *mut _).cast(),
                std::mem::size_of_val(&limits) as u32,
            ) == 0
            {
                CloseHandle(job);
                return None;
            }
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() || AssignProcessToJobObject(job, process) == 0 {
                if !process.is_null() {
                    CloseHandle(process);
                }
                CloseHandle(job);
                return None;
            }
            CloseHandle(process);
            Some(JobHandle(job))
        }
    }
    pub fn terminate(job: &JobHandle) -> Result<(), String> {
        unsafe {
            if TerminateJobObject(job.0, 1) == 0 {
                Err(std::io::Error::last_os_error().to_string())
            } else {
                Ok(())
            }
        }
    }
}
#[cfg(all(test, unix))]
mod tests {
    use super::ProcessTree;
    use std::process::Command;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn attach_tolerates_child_that_exits_before_the_parent_probe() {
        // Regression: the pre_exec setpgid races the parent's attach probe.
        // A child that dies first probes as -1 and used to be misreported as
        // an identity mismatch; attach must only reject a live foreign group.
        let mut command = Command::new("sh");
        command.arg("-c").arg("exit 0");
        super::configure_child(&mut command);
        let mut child = command.spawn().expect("spawn sh");
        let tree = ProcessTree::attach(&mut child);
        assert!(
            tree.is_ok(),
            "attach must not reject an exited child: {tree:?}"
        );
        let mut tree = tree.unwrap();
        let _ = tree.terminate(&mut child);
        let _ = child.wait();
    }

    #[test]
    fn terminate_signals_process_group_after_direct_child_exits() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 30 & exit 0"]);
        super::configure_child(&mut command);
        let mut child = command.spawn().unwrap();
        let pgid = child.id() as libc::pid_t;
        let mut tree = ProcessTree::attach(&mut child).unwrap();
        child.wait().unwrap();

        tree.terminate(&mut child).unwrap();
        thread::sleep(Duration::from_millis(100));
        assert!(!ProcessTree::process_group_alive(pgid));
    }
}
