use tokio::process::Command;

pub const UNIX_NICE: i32 = 10;

#[cfg(windows)]
pub const BELOW_NORMAL_PRIORITY_CLASS: u32 = 0x0000_4000;

#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

pub fn lower(command: &mut Command) -> &mut Command {
    #[cfg(unix)]
    unsafe {
        command.pre_exec(|| {
            if libc::setpriority(libc::PRIO_PROCESS, 0, UNIX_NICE) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    #[cfg(windows)]
    command.creation_flags(BELOW_NORMAL_PRIORITY_CLASS | CREATE_NO_WINDOW);
    command
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[tokio::test]
    async fn lowered_child_runs_at_nice_ten() {
        let mut command = Command::new("sh");
        command.args(["-c", "ps -o ni= -p $$"]);
        lower(&mut command);
        let output = command.output().await.expect("spawn");
        assert!(output.status.success());
        let nice = String::from_utf8_lossy(&output.stdout).trim().to_string();
        assert_eq!(nice, "10");
    }

    #[tokio::test]
    async fn lowered_command_still_spawns() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);
        lower(&mut command);
        let status = command.status().await.expect("spawn");
        assert!(status.success());
    }
}
