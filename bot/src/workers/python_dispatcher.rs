/// Python worker subprocess dispatcher.
///
/// Spawns `python -m worker.application` as a child process,
/// writes JSON requests to stdin, reads JSON responses from stdout.
/// Stderr is forwarded to tracing logs.
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};
use tracing::{debug, error, info, warn};

/// Discover extra PATH entries needed for tools like ffmpeg.
/// Checks FFMPEG_PATH env var first, then common install locations.
fn discover_extra_paths() -> Vec<String> {
    let mut extra = Vec::new();

    // Check explicit env var first
    if let Ok(ffmpeg_path) = std::env::var("FFMPEG_PATH") {
        extra.push(ffmpeg_path);
    }

    // Auto-discover common locations on Windows
    if cfg!(target_os = "windows") {
        if let Ok(local_app) = std::env::var("LOCALAPPDATA") {
            // winget installs ffmpeg here
            let winget_dir = PathBuf::from(&local_app)
                .join("Microsoft")
                .join("WinGet")
                .join("Packages");
            if winget_dir.exists() {
                if let Ok(entries) = std::fs::read_dir(&winget_dir) {
                    for entry in entries.flatten() {
                        let name = entry.file_name().to_string_lossy().to_string();
                        if name.starts_with("Gyan.FFmpeg") {
                            // Find the bin directory inside
                            if let Ok(sub_entries) = std::fs::read_dir(entry.path()) {
                                for sub in sub_entries.flatten() {
                                    let bin = sub.path().join("bin");
                                    if bin.join("ffmpeg.exe").exists() {
                                        extra.push(bin.to_string_lossy().to_string());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Also check common manual install locations
        for path in &[
            r"C:\ffmpeg\bin",
            r"C:\Program Files\ffmpeg\bin",
        ] {
            let p = PathBuf::from(path);
            if p.join("ffmpeg.exe").exists() {
                extra.push(path.to_string());
            }
        }
    }

    // Auto-discover common locations on Linux/macOS
    if cfg!(target_os = "linux") || cfg!(target_os = "macos") {
        let common_paths = [
            "/usr/bin",
            "/usr/local/bin",
            "/snap/bin",
            "/opt/homebrew/bin",
            "/home/linuxbrew/.linuxbrew/bin",
        ];

        for path in &common_paths {
            let p = PathBuf::from(path);
            if p.join("ffmpeg").exists() {
                extra.push(path.to_string());
            }
        }

        // Fallback: try `which ffmpeg` to find its directory
        if extra.is_empty() {
            if let Ok(output) = std::process::Command::new("which")
                .arg("ffmpeg")
                .output()
            {
                if output.status.success() {
                    let ffmpeg_path = String::from_utf8_lossy(&output.stdout).trim().to_string();
                    if let Some(parent) = PathBuf::from(&ffmpeg_path).parent() {
                        let dir = parent.to_string_lossy().to_string();
                        if !dir.is_empty() {
                            extra.push(dir);
                        }
                    }
                }
            }
        }
    }

    extra
}

use hermes_shared::ipc_protocol::{IPCRequest, IPCResponse};
use hermes_shared::errors::{IpcError, HermesError};

/// Manages a Python worker subprocess.
pub struct PythonDispatcher {
    /// Path to the worker directory (containing worker/ package).
    worker_dir: PathBuf,
    /// Python executable path.
    python_bin: String,
    /// Child process handle.
    child: Arc<Mutex<Option<Child>>>,
    /// Sender for writing requests to worker stdin.
    stdin_tx: Arc<Mutex<Option<mpsc::Sender<String>>>>,
    /// Per-task response channels.
    pending: Arc<Mutex<HashMap<String, mpsc::UnboundedSender<IPCResponse>>>>,
    /// Whether the worker is running.
    running: Arc<Mutex<bool>>,
}

impl PythonDispatcher {
    /// Create a new dispatcher.
    pub fn new(worker_dir: PathBuf, python_bin: Option<String>) -> Self {
        let default_python = if cfg!(target_os = "windows") {
            "python".to_string()
        } else {
            "python3".to_string()
        };
        Self {
            worker_dir,
            python_bin: python_bin.unwrap_or(default_python),
            child: Arc::new(Mutex::new(None)),
            stdin_tx: Arc::new(Mutex::new(None)),
            pending: Arc::new(Mutex::new(HashMap::new())),
            running: Arc::new(Mutex::new(false)),
        }
    }

    /// Start the Python worker subprocess.
    pub async fn start(&self) -> Result<(), HermesError> {
        info!("Starting Python worker: bin={:?} dir={:?}", self.python_bin, self.worker_dir);

        // Build augmented PATH with ffmpeg and other tool locations
        let extra_paths = discover_extra_paths();
        let current_path = std::env::var("PATH").unwrap_or_default();
        let sep = if cfg!(target_os = "windows") { ";" } else { ":" };
        let augmented_path = if extra_paths.is_empty() {
            current_path.clone()
        } else {
            let extras = extra_paths.join(sep);
            info!("Adding to worker PATH: {}", extras);
            format!("{}{}{}", current_path, sep, extras)
        };

        let mut child = Command::new(&self.python_bin)
            .arg("-m")
            .arg("worker.application")
            .current_dir(&self.worker_dir)
            .env("PATH", &augmented_path)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| IpcError::SpawnFailed(format!(
                "Failed to spawn Python worker at {:?}: {}",
                self.worker_dir, e
            )))?;

        info!("Python worker spawned (pid: {:?})", child.id());

        // Take ownership of stdio handles
        let stdout = child.stdout.take()
            .ok_or_else(|| IpcError::ReadFailed("No stdout handle".into()))?;
        let stderr = child.stderr.take()
            .ok_or_else(|| IpcError::ReadFailed("No stderr handle".into()))?;
        let stdin = child.stdin.take()
            .ok_or_else(|| IpcError::WriteFailed("No stdin handle".into()))?;

        // Create stdin writer channel
        let (stdin_tx, mut stdin_rx) = mpsc::channel::<String>(100);

        // Stdin writer task
        let _stdin_handle = tokio::spawn(async move {
            let mut stdin = stdin;
            while let Some(line) = stdin_rx.recv().await {
                if let Err(e) = stdin.write_all(line.as_bytes()).await {
                    error!("Failed to write to worker stdin: {}", e);
                    break;
                }
                if let Err(e) = stdin.write_all(b"\n").await {
                    error!("Failed to write newline to worker stdin: {}", e);
                    break;
                }
                if let Err(e) = stdin.flush().await {
                    error!("Failed to flush worker stdin: {}", e);
                    break;
                }
                debug!("Sent to worker: {}", line.chars().take(100).collect::<String>());
            }
            debug!("Stdin writer task ended");
        });

        // Stdout reader task - routes responses to pending task channels
        let pending_clone = self.pending.clone();
        let running_clone = self.running.clone();
        let _stdout_handle = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                match IPCResponse::from_json_line(&line) {
                    Ok(response) => {
                        let task_id = response.task_id.clone();
                        debug!("Received from worker: task={} event={:?}", task_id, response.event);

                        let pending = pending_clone.lock().await;
                        if let Some(tx) = pending.get(&task_id) {
                            if let Err(e) = tx.send(response) {
                                warn!("Failed to route response for task {}: {}", task_id, e);
                            }
                        } else {
                            warn!("No pending handler for task {}", task_id);
                        }
                    }
                    Err(e) => {
                        warn!("Invalid JSON from worker stdout: {} (line: {})", e, &line[..line.len().min(200)]);
                    }
                }
            }
            info!("Worker stdout stream ended");
            *running_clone.lock().await = false;
        });

        // Stderr reader task - forward to tracing
        let _stderr_handle = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                // Forward worker logs as debug-level traces
                debug!(target: "python_worker", "{}", line);
            }
            debug!("Worker stderr stream ended");
        });

        // Store handles
        *self.child.lock().await = Some(child);
        *self.stdin_tx.lock().await = Some(stdin_tx);
        *self.running.lock().await = true;

        info!("Python dispatcher started successfully");
        Ok(())
    }

    /// Send a request and get a channel to receive responses.
    ///
    /// Returns an unbounded receiver that will get all responses for this task_id
    /// (progress updates, then final done/error).
    pub async fn send(
        &self,
        request: &IPCRequest,
    ) -> Result<mpsc::UnboundedReceiver<IPCResponse>, HermesError> {
        if !*self.running.lock().await {
            return Err(IpcError::NotRunning.into());
        }

        let json = request.to_json_line()
            .map_err(|e| IpcError::WriteFailed(e.to_string()))?;

        // Create response channel for this task
        let (tx, rx) = mpsc::unbounded_channel();
        self.pending.lock().await.insert(request.task_id.clone(), tx);

        // Send to stdin writer
        let stdin_tx = self.stdin_tx.lock().await;
        if let Some(tx) = stdin_tx.as_ref() {
            tx.send(json).await
                .map_err(|e| IpcError::WriteFailed(e.to_string()))?;
        } else {
            return Err(IpcError::NotRunning.into());
        }

        Ok(rx)
    }

    /// Send a request and wait for the final response (done or error).
    /// Ignores progress events.
    pub async fn send_and_wait(
        &self,
        request: &IPCRequest,
        timeout_secs: u64,
    ) -> Result<IPCResponse, HermesError> {
        let mut rx = self.send(request).await?;

        let result = tokio::time::timeout(
            std::time::Duration::from_secs(timeout_secs),
            async {
                while let Some(response) = rx.recv().await {
                    if response.is_progress() {
                        continue; // Skip progress, wait for final
                    }
                    return Ok(response);
                }
                Err(HermesError::Ipc(IpcError::ReadFailed("Channel closed".into())))
            },
        )
        .await
        .map_err(|_| HermesError::Ipc(IpcError::Timeout(timeout_secs)))?;

        // Clean up pending entry
        self.pending.lock().await.remove(&request.task_id);

        result
    }

    /// Stop the Python worker process.
    pub async fn stop(&self) -> Result<(), HermesError> {
        info!("Stopping Python worker...");

        // Drop stdin sender to signal EOF
        *self.stdin_tx.lock().await = None;

        // Wait briefly for graceful shutdown, then kill
        if let Some(mut child) = self.child.lock().await.take() {
            let timeout = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                child.wait(),
            )
            .await;

            match timeout {
                Ok(Ok(status)) => {
                    info!("Python worker exited with status: {}", status);
                }
                Ok(Err(e)) => {
                    warn!("Error waiting for worker: {}", e);
                }
                Err(_) => {
                    warn!("Worker did not exit in time, killing...");
                    let _ = child.kill().await;
                }
            }
        }

        *self.running.lock().await = false;
        self.pending.lock().await.clear();
        info!("Python worker stopped");
        Ok(())
    }

    /// Remove a pending task (e.g., on cancellation).
    pub async fn remove_pending(&self, task_id: &str) {
        self.pending.lock().await.remove(task_id);
    }
}

impl Drop for PythonDispatcher {
    fn drop(&mut self) {
        // Best-effort cleanup - can't do async in Drop
        // The child process will be killed when the handle is dropped
    }
}
