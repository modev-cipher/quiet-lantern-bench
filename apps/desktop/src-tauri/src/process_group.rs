use std::io;
use std::time::Duration;

use tokio::process::{Child, Command};

#[cfg(windows)]
mod windows_job {
    use std::collections::HashMap;
    use std::io;
    use std::sync::{Mutex, OnceLock};

    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32,
    };
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, OpenThread, ResumeThread, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
        THREAD_SUSPEND_RESUME,
    };

    struct Job(HANDLE);
    unsafe impl Send for Job {}
    unsafe impl Sync for Job {}

    impl Drop for Job {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    static JOBS: OnceLock<Mutex<HashMap<u32, Job>>> = OnceLock::new();

    fn jobs() -> &'static Mutex<HashMap<u32, Job>> {
        JOBS.get_or_init(|| Mutex::new(HashMap::new()))
    }

    fn create_job() -> io::Result<Job> {
        unsafe {
            let handle = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if handle.is_null() {
                return Err(io::Error::last_os_error());
            }
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            if SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                (&raw const info).cast(),
                std::mem::size_of_val(&info) as u32,
            ) == 0
            {
                let error = io::Error::last_os_error().raw_os_error().unwrap_or(1);
                CloseHandle(handle);
                return Err(io::Error::from_raw_os_error(error));
            }
            Ok(Job(handle))
        }
    }

    unsafe fn resume_thread(pid: u32) -> io::Result<()> {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snapshot == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let mut entry: THREADENTRY32 = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut found = Thread32First(snapshot, &mut entry) != 0;
        while found {
            if entry.th32OwnerProcessID == pid {
                let thread = OpenThread(THREAD_SUSPEND_RESUME, 0, entry.th32ThreadID);
                if !thread.is_null() {
                    let result = ResumeThread(thread);
                    CloseHandle(thread);
                    CloseHandle(snapshot);
                    return if result == u32::MAX {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    };
                }
            }
            found = Thread32Next(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "child thread not found",
        ))
    }

    pub fn assign(pid: u32) -> io::Result<()> {
        unsafe {
            let job = create_job()?;
            let process = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if process.is_null() {
                return Err(io::Error::last_os_error());
            }
            let assigned = AssignProcessToJobObject(job.0, process);
            CloseHandle(process);
            if assigned == 0 {
                return Err(io::Error::last_os_error());
            }
            resume_thread(pid)?;
            jobs().lock().expect("Job Object registry").insert(pid, job);
            Ok(())
        }
    }

    pub fn terminate(pid: u32) {
        if let Some(job) = jobs().lock().expect("Job Object registry").remove(&pid) {
            unsafe {
                TerminateJobObject(job.0, 1);
            }
        }
    }

    pub fn release(pid: u32) {
        jobs().lock().expect("Job Object registry").remove(&pid);
    }
}

pub struct Guard {
    pid: Option<u32>,
}

impl Guard {
    pub fn new(pid: Option<u32>) -> Self {
        Self { pid }
    }

    pub fn disarm(mut self) {
        self.pid = None;
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pid) = self.pid {
            unsafe {
                libc::kill(-(pid as i32), libc::SIGKILL);
            }
        }
        #[cfg(windows)]
        if let Some(pid) = self.pid.take() {
            windows_job::terminate(pid);
        }
    }
}

pub struct OwnedChild {
    child: Option<Child>,
    pid: Option<u32>,
}

impl OwnedChild {
    pub fn new(child: Child) -> Self {
        let pid = child.id();
        Self {
            child: Some(child),
            pid,
        }
    }

    pub async fn wait_with_output(&mut self) -> io::Result<std::process::Output> {
        use tokio::io::AsyncReadExt;

        #[cfg(windows)]
        let pid = self.pid;
        let child = self.child.as_mut().expect("owned child");
        let mut stdout = child.stdout.take();
        let mut stderr = child.stderr.take();
        let stdout_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            if let Some(stream) = stdout.as_mut() {
                stream.read_to_end(&mut bytes).await?;
            }
            Ok::<_, io::Error>(bytes)
        });
        let stderr_task = tokio::spawn(async move {
            let mut bytes = Vec::new();
            if let Some(stream) = stderr.as_mut() {
                stream.read_to_end(&mut bytes).await?;
            }
            Ok::<_, io::Error>(bytes)
        });
        let status = child.wait().await?;
        let stdout = stdout_task.await.map_err(io::Error::other)??;
        let stderr = stderr_task.await.map_err(io::Error::other)??;
        self.pid = None;
        #[cfg(windows)]
        if let Some(pid) = pid {
            windows_job::release(pid);
        }
        self.child = None;
        Ok(std::process::Output {
            status,
            stdout,
            stderr,
        })
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            #[cfg(unix)]
            if let Some(pid) = self.pid.take() {
                unsafe {
                    libc::kill(-(pid as i32), libc::SIGKILL);
                }
            }
            #[cfg(not(unix))]
            if let Some(pid) = self.pid.take() {
                windows_job::terminate(pid);
            }
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    let _ = child.wait().await;
                });
            }
        }
    }
}

pub fn configure(command: &mut Command) -> &mut Command {
    #[cfg(unix)]
    command.process_group(0);
    #[cfg(windows)]
    command.creation_flags(
        windows_sys::Win32::System::Threading::CREATE_SUSPENDED
            | crate::process_priority::BELOW_NORMAL_PRIORITY_CLASS
            | crate::process_priority::CREATE_NO_WINDOW,
    );
    command.kill_on_drop(true)
}

pub fn assign(child: &Child) -> io::Result<()> {
    #[cfg(windows)]
    {
        let pid = child
            .id()
            .ok_or_else(|| io::Error::other("child exited before Job Object assignment"))?;
        windows_job::assign(pid)?;
    }
    #[cfg(not(windows))]
    let _ = child;
    Ok(())
}

pub async fn terminate(child: &mut Child, pid: Option<u32>, grace: Duration) -> io::Result<()> {
    #[cfg(unix)]
    if let Some(pid) = pid {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGTERM);
        }
    }
    if let Ok(status) = tokio::time::timeout(grace, child.wait()).await {
        #[cfg(not(unix))]
        if let Some(pid) = pid {
            windows_job::terminate(pid);
        }
        return status.map(|_| ());
    }
    #[cfg(unix)]
    if let Some(pid) = pid {
        unsafe {
            libc::kill(-(pid as i32), libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    if let Some(pid) = pid {
        windows_job::terminate(pid);
    }
    child.wait().await.map(|_| ())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn guard_kills_entire_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        let mut command = Command::new("sh");
        command.arg("-c").arg(format!(
            "sleep 60 & printf %s $! > '{}'; wait",
            pid_file.display()
        ));
        configure(&mut command);
        let child = command.spawn().unwrap();
        let guard = Guard::new(child.id());
        for _ in 0..100 {
            if pid_file.is_file() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let grandchild: i32 = tokio::fs::read_to_string(&pid_file)
            .await
            .unwrap()
            .parse()
            .unwrap();
        drop(guard);
        tokio::time::sleep(Duration::from_millis(50)).await;
        let alive = unsafe { libc::kill(grandchild, 0) } == 0;
        assert!(
            !alive,
            "grandchild {grandchild} survived process-group guard"
        );
    }
}
