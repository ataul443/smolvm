//! Sandbox supervisor for health monitoring and restart policies.
//!
//! The supervisor runs as a background task that periodically checks sandbox health
//! and automatically restarts sandboxes based on their restart policies.

use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

use crate::api::state::{ensure_sandbox_running, ApiState};
use crate::config::{RecordState, RestartConfig, RestartPolicy};

/// Interval between health checks.
const CHECK_INTERVAL: Duration = Duration::from_secs(5);

/// Maximum backoff delay in seconds.
const MAX_BACKOFF_SECS: u64 = 300;

/// Minimum delay between restart attempts.
const MIN_RESTART_DELAY: Duration = Duration::from_secs(1);

/// Sandbox supervisor for health monitoring and automatic restarts.
pub struct Supervisor {
    state: Arc<ApiState>,
    shutdown_rx: watch::Receiver<bool>,
}

impl Supervisor {
    /// Create a new supervisor.
    pub fn new(state: Arc<ApiState>, shutdown_rx: watch::Receiver<bool>) -> Self {
        Self { state, shutdown_rx }
    }

    /// Run the supervisor loop.
    ///
    /// This method blocks until shutdown is signaled.
    pub async fn run(mut self) {
        let mut ticker = tokio::time::interval(CHECK_INTERVAL);
        // Don't catch up on missed ticks
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

        tracing::info!("supervisor started");

        loop {
            tokio::select! {
                _ = ticker.tick() => {
                    self.check_all_sandboxes().await;
                }
                _ = self.shutdown_rx.changed() => {
                    if *self.shutdown_rx.borrow() {
                        tracing::info!("supervisor shutting down");
                        break;
                    }
                }
            }
        }
    }

    /// Check all sandboxes and restart any that need it.
    async fn check_all_sandboxes(&self) {
        let sandbox_names = self.state.list_sandbox_names();

        for name in sandbox_names {
            if let Err(e) = self.check_sandbox(&name).await {
                tracing::warn!(sandbox = %name, error = %e, "failed to check sandbox");
            }
        }

        // Also rotate logs for all sandboxes
        self.rotate_logs_if_needed().await;
    }

    /// Check a single sandbox and restart if needed.
    async fn check_sandbox(&self, name: &str) -> crate::Result<()> {
        // Check if sandbox is alive
        let is_alive = self.state.is_sandbox_alive(name);

        if is_alive {
            // Sandbox is running, nothing to do
            return Ok(());
        }

        // Sandbox is dead — try to retrieve its exit code via waitpid
        // and persist it so the restart policy can use it.
        if let Ok(Some(record)) = self.state.db().get_vm(name) {
            if let Some(pid) = record.pid {
                let exit_code = crate::process::try_wait(pid);
                self.state.set_last_exit_code(name, exit_code);
            }
        }

        let last_exit_code = self.state.get_last_exit_code(name);

        // Sandbox is dead, check restart policy
        let restart_config = match self.state.get_restart_config(name) {
            Some(config) => config,
            None => return Ok(()), // Sandbox doesn't exist anymore
        };

        // Determine if we should restart
        if !Self::should_restart(&restart_config, last_exit_code) {
            tracing::debug!(sandbox = %name, policy = %restart_config.policy, "sandbox dead, not restarting per policy");
            // Update state to stopped (best-effort in supervisor)
            if let Err(e) = self
                .state
                .update_sandbox_state(name, RecordState::Stopped, None)
            {
                tracing::warn!(sandbox = %name, error = %e, "failed to persist stopped state");
            }
            return Ok(());
        }

        // Calculate backoff delay
        let backoff_secs = Self::calculate_backoff(restart_config.restart_count);
        let backoff = Duration::from_secs(backoff_secs);

        tracing::info!(
            sandbox = %name,
            restart_count = restart_config.restart_count,
            backoff_secs = backoff_secs,
            "sandbox dead, scheduling restart"
        );

        // Wait for backoff (but check for shutdown during wait)
        if backoff > MIN_RESTART_DELAY {
            tokio::time::sleep(backoff).await;
        }

        // Increment restart count
        self.state.increment_restart_count(name);

        // Attempt restart
        self.restart_sandbox(name).await
    }

    /// Attempt to restart a sandbox.
    async fn restart_sandbox(&self, name: &str) -> crate::Result<()> {
        let entry = match self.state.get_sandbox(name) {
            Ok(entry) => entry,
            Err(_) => {
                tracing::warn!(sandbox = %name, "sandbox no longer exists, skipping restart");
                return Ok(());
            }
        };

        let start_result = ensure_sandbox_running(&entry).await;

        // Handle start result
        match start_result {
            Ok(()) => {
                // Get updated PID and persist state
                let pid = {
                    let entry = entry.lock();
                    entry.manager.child_pid()
                };
                if let Err(e) = self
                    .state
                    .update_sandbox_state(name, RecordState::Running, pid)
                {
                    tracing::warn!(sandbox = %name, error = %e, "failed to persist running state");
                }
                tracing::info!(sandbox = %name, pid = ?pid, "sandbox restarted successfully");
                Ok(())
            }
            Err(e) => {
                if let Err(db_err) =
                    self.state
                        .update_sandbox_state(name, RecordState::Failed, None)
                {
                    tracing::warn!(sandbox = %name, error = %db_err, "failed to persist failed state");
                }
                tracing::error!(sandbox = %name, error = %e, "failed to restart sandbox");
                Err(e)
            }
        }
    }

    /// Determine if a sandbox should be restarted based on its restart configuration.
    fn should_restart(config: &RestartConfig, last_exit_code: Option<i32>) -> bool {
        // Check max retries limit
        if config.max_retries > 0 && config.restart_count >= config.max_retries {
            return false;
        }

        match config.policy {
            RestartPolicy::Never => false,
            RestartPolicy::Always => true,
            RestartPolicy::OnFailure => {
                // Only restart if the process exited with a non-zero exit code.
                // Exit code 0 means clean exit — no restart needed.
                last_exit_code != Some(0)
            }
            RestartPolicy::UnlessStopped => !config.user_stopped,
        }
    }

    /// Calculate exponential backoff delay based on restart count.
    fn calculate_backoff(restart_count: u32) -> u64 {
        // Exponential backoff: 2^n seconds, capped at MAX_BACKOFF_SECS
        let exponent = restart_count.min(8); // Prevent overflow
        (2u64.pow(exponent)).min(MAX_BACKOFF_SECS)
    }

    /// Rotate logs for all sandboxes if they exceed the size limit.
    async fn rotate_logs_if_needed(&self) {
        let sandbox_names = self.state.list_sandbox_names();

        for name in sandbox_names {
            if let Some(log_path) = self.get_sandbox_log_path(&name) {
                if let Err(e) = crate::log_rotation::rotate_if_needed(&log_path) {
                    tracing::debug!(sandbox = %name, error = %e, "failed to rotate logs");
                }
            }
        }
    }

    /// Get the console log path for a sandbox.
    fn get_sandbox_log_path(&self, name: &str) -> Option<std::path::PathBuf> {
        let log_path = crate::paths::runtime_dir()
            .join("vms")
            .join(name)
            .join("agent-console.log");

        if log_path.exists() {
            Some(log_path)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_restart() {
        // (policy, max_retries, restart_count, user_stopped, last_exit_code, expected, description)
        let cases = [
            (
                RestartPolicy::Never,
                0,
                0,
                false,
                None,
                false,
                "never policy",
            ),
            (
                RestartPolicy::Always,
                0,
                5,
                false,
                None,
                true,
                "always policy",
            ),
            (
                RestartPolicy::Always,
                3,
                3,
                false,
                None,
                false,
                "max retries reached",
            ),
            (
                RestartPolicy::Always,
                3,
                2,
                false,
                None,
                true,
                "under max retries",
            ),
            (
                RestartPolicy::OnFailure,
                0,
                0,
                false,
                Some(1),
                true,
                "on-failure with non-zero exit",
            ),
            (
                RestartPolicy::OnFailure,
                0,
                0,
                false,
                Some(0),
                false,
                "on-failure with clean exit",
            ),
            (
                RestartPolicy::OnFailure,
                0,
                0,
                false,
                None,
                true,
                "on-failure with unknown exit code",
            ),
            (
                RestartPolicy::UnlessStopped,
                0,
                0,
                false,
                None,
                true,
                "unless-stopped running",
            ),
            (
                RestartPolicy::UnlessStopped,
                0,
                0,
                true,
                None,
                false,
                "unless-stopped user stopped",
            ),
        ];

        for (policy, max_retries, restart_count, user_stopped, last_exit_code, expected, desc) in
            cases
        {
            let config = RestartConfig {
                policy,
                max_retries,
                restart_count,
                user_stopped,
            };
            assert_eq!(
                Supervisor::should_restart(&config, last_exit_code),
                expected,
                "{}",
                desc
            );
        }
    }

    #[test]
    fn test_calculate_backoff() {
        assert_eq!(Supervisor::calculate_backoff(0), 1);
        assert_eq!(Supervisor::calculate_backoff(1), 2);
        assert_eq!(Supervisor::calculate_backoff(2), 4);
        assert_eq!(Supervisor::calculate_backoff(3), 8);
        assert_eq!(Supervisor::calculate_backoff(8), 256);
        // Exponent is capped at 8, so 2^8 = 256 for any count >= 8
        assert_eq!(Supervisor::calculate_backoff(9), 256);
        assert_eq!(Supervisor::calculate_backoff(10), 256);
        assert_eq!(Supervisor::calculate_backoff(100), 256);
    }
}
