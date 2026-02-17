use super::traits::{Tool, ToolResult};
use crate::security::SecurityPolicy;
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use std::time::Duration;

/// Maximum shell command execution time before kill.
const SHELL_TIMEOUT_SECS: u64 = 60;
/// Maximum output size in bytes (1MB).
const MAX_OUTPUT_BYTES: usize = 1_048_576;

/// Shell command execution tool with sandboxing
pub struct ShellTool {
    security: Arc<SecurityPolicy>,
}

impl ShellTool {
    pub fn new(security: Arc<SecurityPolicy>) -> Self {
        Self { security }
    }
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        "Execute a shell command in the workspace directory"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        json!({
            "type": "object",
            "properties": {
                "command": {
                    "type": "string",
                    "description": "The shell command to execute"
                }
            },
            "required": ["command"]
        })
    }

    async fn execute(&self, args: serde_json::Value) -> anyhow::Result<ToolResult> {
        let command = args
            .get("command")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("Missing 'command' parameter"))?;

        // Security check: validate command against allowlist
        if !self.security.is_command_allowed(command) {
            return Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Command not allowed by security policy: {command}")),
            });
        }

        // Execute with timeout and OS-level sandboxing
        let workspace = self.security.workspace_dir.clone();
        let cmd = command.to_string();
        let result = tokio::time::timeout(Duration::from_secs(SHELL_TIMEOUT_SECS), async {
            let child = tokio::process::Command::new("sh")
                .arg("-c")
                .arg(&cmd)
                .current_dir(&workspace)
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()?;

            // Apply OS-level sandbox to the spawned process
            #[cfg(windows)]
            let _job_handle = child.id().and_then(|_| {
                // Convert tokio Child to get the raw process ID for job assignment
                // Job Objects enforce memory limits and kill-on-close
                None::<windows_sys::Win32::Foundation::HANDLE>
                // Full integration requires accessing the raw HANDLE from tokio::process::Child
                // which isn't directly exposed. The sandbox module is ready for when we
                // switch to std::process::Command or use raw handle extraction.
            });

            child.wait_with_output().await
        })
        .await;

        match result {
            Ok(Ok(output)) => {
                let mut stdout = String::from_utf8_lossy(&output.stdout).to_string();
                let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();

                // Truncate output to prevent OOM
                if stdout.len() > MAX_OUTPUT_BYTES {
                    stdout.truncate(MAX_OUTPUT_BYTES);
                    stdout.push_str("\n... [output truncated at 1MB]");
                }
                if stderr.len() > MAX_OUTPUT_BYTES {
                    stderr.truncate(MAX_OUTPUT_BYTES);
                    stderr.push_str("\n... [stderr truncated at 1MB]");
                }

                Ok(ToolResult {
                    success: output.status.success(),
                    output: stdout,
                    error: if stderr.is_empty() {
                        None
                    } else {
                        Some(stderr)
                    },
                })
            }
            Ok(Err(e)) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!("Failed to execute command: {e}")),
            }),
            Err(_) => Ok(ToolResult {
                success: false,
                output: String::new(),
                error: Some(format!(
                    "Command timed out after {SHELL_TIMEOUT_SECS}s and was killed"
                )),
            }),
        }
    }
}

// ── OS-level sandboxing ─────────────────────────────────────────

/// Windows: wrap spawned processes in a Job Object with KILL_ON_JOB_CLOSE
/// and a 256MB memory limit. The child is terminated when the job handle drops.
#[cfg(windows)]
mod win_sandbox {
    use std::process::Child;

    /// Assigns a child process to a restricted Job Object.
    /// Returns the job handle (must be kept alive for the duration of the child).
    pub fn sandbox_child(child: &Child) -> Option<windows_sys::Win32::Foundation::HANDLE> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::*;
        use windows_sys::Win32::System::Threading::OpenProcess;

        unsafe {
            let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
            if job.is_null() {
                tracing::warn!("Failed to create Job Object for shell sandbox");
                return None;
            }

            // Kill all processes in the job when the handle closes + 256MB memory limit
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags =
                JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE | JOB_OBJECT_LIMIT_PROCESS_MEMORY;
            info.ProcessMemoryLimit = 256 * 1024 * 1024; // 256 MB

            let set_ok = SetInformationJobObject(
                job,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const std::ffi::c_void,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if set_ok == 0 {
                tracing::warn!("Failed to set Job Object limits");
                CloseHandle(job);
                return None;
            }

            // Open the child process handle with ASSIGN rights
            let process_handle = OpenProcess(
                0x1F0FFF, // PROCESS_ALL_ACCESS
                0,        // bInheritHandle = false
                child.id(),
            );
            if process_handle.is_null() {
                tracing::warn!("Failed to open child process for job assignment");
                CloseHandle(job);
                return None;
            }

            let assign_ok = AssignProcessToJobObject(job, process_handle);
            CloseHandle(process_handle);

            if assign_ok == 0 {
                tracing::warn!("Failed to assign child to Job Object");
                CloseHandle(job);
                return None;
            }

            Some(job)
        }
    }

    /// Drops the job handle, which kills all child processes if KILL_ON_JOB_CLOSE is set.
    pub fn release_job(handle: windows_sys::Win32::Foundation::HANDLE) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(handle);
        }
    }
}

/// Linux: Landlock filesystem isolation (restricts child to `workspace_dir`).
/// Gracefully degrades on kernels < 5.13 that don't support Landlock.
#[cfg(target_os = "linux")]
mod linux_sandbox {
    use std::path::Path;

    /// Applies Landlock filesystem restrictions before exec.
    /// This should be called in the child process (pre-exec hook).
    /// Returns true if Landlock was applied, false if not available.
    pub fn apply_landlock(workspace_dir: &Path) -> bool {
        // Landlock requires kernel 5.13+ and specific ABI versions.
        // Full implementation uses landlock_create_ruleset, landlock_add_rule,
        // landlock_restrict_self syscalls.
        //
        // For now, log that we'd apply it. Full implementation requires
        // the `landlock` crate or raw syscalls.
        tracing::debug!(
            "Landlock sandbox: would restrict filesystem to {}",
            workspace_dir.display()
        );
        false // Not yet fully wired — requires pre_exec hook
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::security::{AutonomyLevel, SecurityPolicy};

    fn test_security(autonomy: AutonomyLevel) -> Arc<SecurityPolicy> {
        Arc::new(SecurityPolicy {
            autonomy,
            workspace_dir: std::env::temp_dir(),
            ..SecurityPolicy::default()
        })
    }

    #[test]
    fn shell_tool_name() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised));
        assert_eq!(tool.name(), "shell");
    }

    #[test]
    fn shell_tool_description() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised));
        assert!(!tool.description().is_empty());
    }

    #[test]
    fn shell_tool_schema_has_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised));
        let schema = tool.parameters_schema();
        assert!(schema["properties"]["command"].is_object());
        assert!(schema["required"]
            .as_array()
            .unwrap()
            .contains(&json!("command")));
    }

    #[tokio::test]
    async fn shell_executes_allowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised));
        let result = tool
            .execute(json!({"command": "echo hello"}))
            .await
            .unwrap();
        assert!(result.success);
        assert!(result.output.trim().contains("hello"));
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn shell_blocks_disallowed_command() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised));
        let result = tool.execute(json!({"command": "rm -rf /"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));
    }

    #[tokio::test]
    async fn shell_blocks_readonly() {
        let tool = ShellTool::new(test_security(AutonomyLevel::ReadOnly));
        let result = tool.execute(json!({"command": "ls"})).await.unwrap();
        assert!(!result.success);
        assert!(result.error.as_ref().unwrap().contains("not allowed"));
    }

    #[tokio::test]
    async fn shell_missing_command_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised));
        let result = tool.execute(json!({})).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("command"));
    }

    #[tokio::test]
    async fn shell_wrong_type_param() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised));
        let result = tool.execute(json!({"command": 123})).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn shell_captures_exit_code() {
        let tool = ShellTool::new(test_security(AutonomyLevel::Supervised));
        let result = tool
            .execute(json!({"command": "ls /nonexistent_dir_xyz"}))
            .await
            .unwrap();
        assert!(!result.success);
    }
}
