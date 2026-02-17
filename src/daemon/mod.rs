use crate::config::Config;
use anyhow::{Context, Result};
use chrono::Utc;
use fs2::FileExt;
use std::future::Future;
use std::path::PathBuf;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Duration;

const STATUS_FLUSH_SECONDS: u64 = 5;

#[allow(clippy::too_many_lines)]
pub async fn run(config: Config, host: String, port: u16) -> Result<()> {
    // Acquire exclusive lock to prevent concurrent daemon instances
    let lock_path = config
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("daemon.lock");
    if let Some(parent) = lock_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let lock_file =
        std::fs::File::create(&lock_path).context("Failed to create daemon lock file")?;
    lock_file.try_lock_exclusive().map_err(|_| {
        anyhow::anyhow!(
            "{}",
            crate::health::structured_error(
                "Failed to start daemon",
                &format!("another instance holds the lock ({})", lock_path.display()),
                "stop the existing daemon with Ctrl+C or remove the lock file"
            )
        )
    })?;
    // Lock held for lifetime of `lock_file` — released on drop at function exit

    let initial_backoff = config.reliability.channel_initial_backoff_secs.max(1);
    let max_backoff = config
        .reliability
        .channel_max_backoff_secs
        .max(initial_backoff);

    crate::health::mark_component_ok("daemon");

    if config.heartbeat.enabled {
        let _ =
            crate::heartbeat::engine::HeartbeatEngine::ensure_heartbeat_file(&config.workspace_dir)
                .await;
    }

    let mut tasks = JoinSet::new();
    tasks.spawn(run_state_writer(config.clone()));

    {
        let gateway_cfg = config.clone();
        let gateway_host = host.clone();
        tasks.spawn(run_supervised_component(
            "gateway",
            initial_backoff,
            max_backoff,
            move || {
                let cfg = gateway_cfg.clone();
                let host = gateway_host.clone();
                async move { crate::gateway::run_gateway(&host, port, cfg).await }
            },
        ));
    }

    {
        if has_supervised_channels(&config) {
            let channels_cfg = config.clone();
            tasks.spawn(run_supervised_component(
                "channels",
                initial_backoff,
                max_backoff,
                move || {
                    let cfg = channels_cfg.clone();
                    async move { crate::channels::start_channels(cfg).await }
                },
            ));
        } else {
            crate::health::mark_component_ok("channels");
            tracing::info!("No real-time channels configured; channel supervisor disabled");
        }
    }

    if config.heartbeat.enabled {
        let heartbeat_cfg = config.clone();
        tasks.spawn(run_supervised_component(
            "heartbeat",
            initial_backoff,
            max_backoff,
            move || {
                let cfg = heartbeat_cfg.clone();
                async move { run_heartbeat_worker(cfg).await }
            },
        ));
    }

    {
        let scheduler_cfg = config.clone();
        tasks.spawn(run_supervised_component(
            "scheduler",
            initial_backoff,
            max_backoff,
            move || {
                let cfg = scheduler_cfg.clone();
                async move { crate::cron::scheduler::run(cfg).await }
            },
        ));
    }

    // Periodic working set trimming on Windows (releases unused physical pages)
    #[cfg(windows)]
    tasks.spawn(async {
        let mut interval = tokio::time::interval(Duration::from_secs(300)); // every 5 min
        loop {
            interval.tick().await;
            // SetProcessWorkingSetSize with (SIZE_MAX, SIZE_MAX) trims the working set
            unsafe {
                let process = windows_sys::Win32::System::Threading::GetCurrentProcess();
                windows_sys::Win32::System::Threading::SetProcessWorkingSetSize(
                    process,
                    usize::MAX,
                    usize::MAX,
                );
            }
            tracing::debug!("Trimmed process working set");
        }
    });

    println!("🧠 Baihu daemon started");
    println!("   Gateway:  http://{host}:{port}");
    println!("   Components: gateway, channels, heartbeat, scheduler");
    println!("   Ctrl+C to stop");

    tokio::signal::ctrl_c().await?;
    crate::health::mark_component_error("daemon", "shutdown requested");

    tasks.abort_all();
    while tasks.join_next().await.is_some() {}

    Ok(())
}

pub fn state_file_path(config: &Config) -> PathBuf {
    config
        .config_path
        .parent()
        .map_or_else(|| PathBuf::from("."), PathBuf::from)
        .join("daemon_state.json")
}

async fn run_state_writer(config: Config) {
    let path = state_file_path(&config);
    if let Some(parent) = path.parent() {
        let _ = tokio::fs::create_dir_all(parent).await;
    }

    let mut interval = tokio::time::interval(Duration::from_secs(STATUS_FLUSH_SECONDS));
    loop {
        interval.tick().await;
        let mut json = crate::health::snapshot_json();
        if let Some(obj) = json.as_object_mut() {
            obj.insert(
                "written_at".into(),
                serde_json::json!(Utc::now().to_rfc3339()),
            );
        }
        let data = serde_json::to_vec_pretty(&json).unwrap_or_else(|_| b"{}".to_vec());
        let _ = crate::security::atomic_write::atomic_write_async(&path, data).await;
    }
}

async fn run_supervised_component<F, Fut>(
    name: &'static str,
    initial_backoff_secs: u64,
    max_backoff_secs: u64,
    mut run_component: F,
) where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    let mut backoff = initial_backoff_secs.max(1);
    let max_backoff = max_backoff_secs.max(backoff);

    loop {
        crate::health::mark_component_ok(name);
        match run_component().await {
            Ok(()) => {
                crate::health::mark_component_error(name, "component exited unexpectedly");
                tracing::warn!("Daemon component '{name}' exited unexpectedly");
            }
            Err(e) => {
                crate::health::mark_component_error(name, e.to_string());
                tracing::error!("Daemon component '{name}' failed: {e}");
            }
        }

        crate::health::bump_component_restart(name);
        // +/-25% jitter to prevent thundering herd on mass restart
        let jitter_bytes = uuid::Uuid::new_v4();
        let raw = u32::from_le_bytes([
            jitter_bytes.as_bytes()[0],
            jitter_bytes.as_bytes()[1],
            jitter_bytes.as_bytes()[2],
            jitter_bytes.as_bytes()[3],
        ]);
        let factor = 0.75 + (f64::from(raw) / f64::from(u32::MAX)) * 0.5;
        #[allow(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            clippy::cast_precision_loss
        )]
        let jittered = ((backoff as f64) * factor) as u64;
        tokio::time::sleep(Duration::from_secs(jittered.max(1))).await;
        backoff = backoff.saturating_mul(2).min(max_backoff);
    }
}

fn spawn_component_supervisor<F, Fut>(
    name: &'static str,
    initial_backoff_secs: u64,
    max_backoff_secs: u64,
    run_component: F,
) -> JoinHandle<()>
where
    F: FnMut() -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send + 'static,
{
    tokio::spawn(run_supervised_component(
        name,
        initial_backoff_secs,
        max_backoff_secs,
        run_component,
    ))
}

async fn run_heartbeat_worker(config: Config) -> Result<()> {
    let observer: std::sync::Arc<dyn crate::observability::Observer> =
        std::sync::Arc::from(crate::observability::create_observer(&config.observability));
    let engine = crate::heartbeat::engine::HeartbeatEngine::new(
        config.heartbeat.clone(),
        config.workspace_dir.clone(),
        observer,
    );

    let interval_mins = config.heartbeat.interval_minutes.max(5);
    let mut interval = tokio::time::interval(Duration::from_secs(u64::from(interval_mins) * 60));

    loop {
        interval.tick().await;

        let tasks = engine.collect_tasks().await?;
        if tasks.is_empty() {
            continue;
        }

        for task in tasks {
            let prompt = format!("[Heartbeat Task] {task}");
            let temp = config.default_temperature;
            if let Err(e) = crate::agent::run(config.clone(), Some(prompt), None, None, temp).await
            {
                crate::health::mark_component_error("heartbeat", e.to_string());
                tracing::warn!("Heartbeat task failed: {e}");
            } else {
                crate::health::mark_component_ok("heartbeat");
            }
        }
    }
}

fn has_supervised_channels(config: &Config) -> bool {
    config.channels_config.telegram.is_some()
        || config.channels_config.discord.is_some()
        || config.channels_config.slack.is_some()
        || config.channels_config.imessage.is_some()
        || config.channels_config.matrix.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use fs2::FileExt;
    use tempfile::TempDir;

    fn test_config(tmp: &TempDir) -> Config {
        let mut config = Config::default();
        config.workspace_dir = tmp.path().join("workspace");
        config.config_path = tmp.path().join("config.toml");
        std::fs::create_dir_all(&config.workspace_dir).unwrap();
        config
    }

    #[test]
    fn state_file_path_uses_config_directory() {
        let tmp = TempDir::new().unwrap();
        let config = test_config(&tmp);

        let path = state_file_path(&config);
        assert_eq!(path, tmp.path().join("daemon_state.json"));
    }

    #[tokio::test]
    async fn supervisor_marks_error_and_restart_on_failure() {
        let handle = spawn_component_supervisor("daemon-test-fail", 1, 1, || async {
            anyhow::bail!("boom")
        });

        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.abort();
        let _ = handle.await;

        let snapshot = crate::health::snapshot_json();
        let component = &snapshot["components"]["daemon-test-fail"];
        assert_eq!(component["status"], "error");
        assert!(component["restart_count"].as_u64().unwrap_or(0) >= 1);
        assert!(component["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("boom"));
    }

    #[tokio::test]
    async fn supervisor_marks_unexpected_exit_as_error() {
        let handle = spawn_component_supervisor("daemon-test-exit", 1, 1, || async { Ok(()) });

        tokio::time::sleep(Duration::from_millis(50)).await;
        handle.abort();
        let _ = handle.await;

        let snapshot = crate::health::snapshot_json();
        let component = &snapshot["components"]["daemon-test-exit"];
        assert_eq!(component["status"], "error");
        assert!(component["restart_count"].as_u64().unwrap_or(0) >= 1);
        assert!(component["last_error"]
            .as_str()
            .unwrap_or("")
            .contains("component exited unexpectedly"));
    }

    #[test]
    fn exclusive_lock_prevents_second_acquisition() {
        let tmp = TempDir::new().unwrap();
        let lock_path = tmp.path().join("daemon.lock");

        let f1 = std::fs::File::create(&lock_path).unwrap();
        f1.try_lock_exclusive().unwrap();

        let f2 = std::fs::File::open(&lock_path).unwrap();
        assert!(
            f2.try_lock_exclusive().is_err(),
            "Second exclusive lock should fail while first is held"
        );
        // Drop f1 releases the lock
        drop(f1);
        // Now f2 should be able to acquire
        let f3 = std::fs::File::open(&lock_path).unwrap();
        assert!(
            f3.try_lock_exclusive().is_ok(),
            "Lock should succeed after first holder drops"
        );
    }

    #[test]
    fn detects_no_supervised_channels() {
        let config = Config::default();
        assert!(!has_supervised_channels(&config));
    }

    #[test]
    fn detects_supervised_channels_present() {
        let mut config = Config::default();
        config.channels_config.telegram = Some(crate::config::TelegramConfig {
            bot_token: "token".into(),
            allowed_users: vec![],
        });
        assert!(has_supervised_channels(&config));
    }
}
