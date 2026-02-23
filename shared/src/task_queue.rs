/// Concurrent task queue for managing download operations.
///
/// Uses tokio Semaphore to limit concurrency and track active tasks.
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, Semaphore, OwnedSemaphorePermit};
use tracing::{info, warn};
use chrono::Utc;

/// Status of a tracked task in the queue.
#[derive(Debug, Clone)]
pub struct TrackedTask {
    pub task_id: String,
    pub chat_id: i64,
    pub task_type: String,
    pub status: TaskState,
    pub progress: u8,
    pub speed: Option<String>,
    pub enqueued_at: chrono::DateTime<Utc>,
    pub started_at: Option<chrono::DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskState {
    Queued,
    Running,
    Done,
    Failed,
    Cancelled,
}

/// Main task queue with concurrency control.
pub struct TaskQueue {
    /// Semaphore to limit concurrent tasks.
    semaphore: Arc<Semaphore>,
    /// Active permits (held while task runs).
    permits: Arc<Mutex<HashMap<String, OwnedSemaphorePermit>>>,
    /// Tracked task metadata.
    tasks: Arc<Mutex<HashMap<String, TrackedTask>>>,
    /// Max concurrent tasks.
    max_concurrent: usize,
}

impl TaskQueue {
    /// Create a new task queue with the given concurrency limit.
    pub fn new(max_concurrent: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_concurrent)),
            permits: Arc::new(Mutex::new(HashMap::new())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            max_concurrent,
        }
    }

    /// Enqueue a task. Returns false if already tracked.
    pub async fn enqueue(&self, task_id: &str, chat_id: i64, task_type: &str) -> bool {
        let mut tasks = self.tasks.lock().await;
        if tasks.contains_key(task_id) {
            warn!("Task {} already in queue", task_id);
            return false;
        }

        tasks.insert(task_id.to_string(), TrackedTask {
            task_id: task_id.to_string(),
            chat_id,
            task_type: task_type.to_string(),
            status: TaskState::Queued,
            progress: 0,
            speed: None,
            enqueued_at: Utc::now(),
            started_at: None,
        });

        info!("Task {} enqueued (type: {})", task_id, task_type);
        true
    }

    /// Acquire a concurrency permit. Waits if at capacity.
    pub async fn acquire(&self, task_id: &str) -> bool {
        let permit = match self.semaphore.clone().acquire_owned().await {
            Ok(p) => p,
            Err(_) => {
                warn!("Semaphore closed for task {}", task_id);
                return false;
            }
        };

        // Store permit and mark running
        self.permits.lock().await.insert(task_id.to_string(), permit);
        if let Some(task) = self.tasks.lock().await.get_mut(task_id) {
            task.status = TaskState::Running;
            task.started_at = Some(Utc::now());
        }

        info!("Task {} acquired slot, now running", task_id);
        true
    }

    /// Update progress for a running task.
    pub async fn update_progress(&self, task_id: &str, percent: u8, speed: Option<String>) {
        if let Some(task) = self.tasks.lock().await.get_mut(task_id) {
            task.progress = percent;
            task.speed = speed;
        }
    }

    /// Mark task as completed and release its permit.
    pub async fn complete(&self, task_id: &str) {
        if let Some(task) = self.tasks.lock().await.get_mut(task_id) {
            task.status = TaskState::Done;
            task.progress = 100;
        }
        // Drop the permit to free the slot
        self.permits.lock().await.remove(task_id);
        info!("Task {} completed, slot released", task_id);
    }

    /// Mark task as failed and release its permit.
    pub async fn fail(&self, task_id: &str) {
        if let Some(task) = self.tasks.lock().await.get_mut(task_id) {
            task.status = TaskState::Failed;
        }
        self.permits.lock().await.remove(task_id);
        warn!("Task {} failed, slot released", task_id);
    }

    /// Cancel a task (removes from queue, releases permit if held).
    pub async fn cancel(&self, task_id: &str) -> bool {
        let mut tasks = self.tasks.lock().await;
        if let Some(task) = tasks.get_mut(task_id) {
            task.status = TaskState::Cancelled;
            drop(tasks);
            self.permits.lock().await.remove(task_id);
            info!("Task {} cancelled", task_id);
            true
        } else {
            false
        }
    }

    /// Get the current status of a task.
    pub async fn get_status(&self, task_id: &str) -> Option<TrackedTask> {
        self.tasks.lock().await.get(task_id).cloned()
    }

    /// Get all tasks for a specific chat.
    pub async fn get_user_tasks(&self, chat_id: i64) -> Vec<TrackedTask> {
        self.tasks.lock().await
            .values()
            .filter(|t| t.chat_id == chat_id)
            .cloned()
            .collect()
    }

    /// Get count of currently running tasks.
    pub async fn running_count(&self) -> usize {
        self.permits.lock().await.len()
    }

    /// Get count of queued (waiting) tasks.
    pub async fn queued_count(&self) -> usize {
        self.tasks.lock().await
            .values()
            .filter(|t| t.status == TaskState::Queued)
            .count()
    }

    /// Get queue statistics.
    pub async fn stats(&self) -> QueueStats {
        let tasks = self.tasks.lock().await;
        let running = self.permits.lock().await.len();
        QueueStats {
            max_concurrent: self.max_concurrent,
            running,
            queued: tasks.values().filter(|t| t.status == TaskState::Queued).count(),
            completed: tasks.values().filter(|t| t.status == TaskState::Done).count(),
            failed: tasks.values().filter(|t| t.status == TaskState::Failed).count(),
            total_tracked: tasks.len(),
        }
    }

    /// Remove completed/failed tasks older than the retention period.
    pub async fn cleanup_old(&self, max_age_secs: i64) {
        let cutoff = Utc::now() - chrono::Duration::seconds(max_age_secs);
        let mut tasks = self.tasks.lock().await;
        tasks.retain(|_, t| {
            t.status == TaskState::Queued
                || t.status == TaskState::Running
                || t.enqueued_at > cutoff
        });
    }
}

/// Queue statistics snapshot.
#[derive(Debug, Clone, serde::Serialize)]
pub struct QueueStats {
    pub max_concurrent: usize,
    pub running: usize,
    pub queued: usize,
    pub completed: usize,
    pub failed: usize,
    pub total_tracked: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_enqueue_and_acquire() {
        let queue = TaskQueue::new(2);
        assert!(queue.enqueue("t1", 123, "youtube").await);
        assert!(queue.acquire("t1").await);
        assert_eq!(queue.running_count().await, 1);
    }

    #[tokio::test]
    async fn test_complete_releases_slot() {
        let queue = TaskQueue::new(1);
        queue.enqueue("t1", 123, "youtube").await;
        queue.acquire("t1").await;
        assert_eq!(queue.running_count().await, 1);

        queue.complete("t1").await;
        assert_eq!(queue.running_count().await, 0);
    }

    #[tokio::test]
    async fn test_duplicate_enqueue() {
        let queue = TaskQueue::new(2);
        assert!(queue.enqueue("t1", 123, "youtube").await);
        assert!(!queue.enqueue("t1", 123, "youtube").await);
    }

    #[tokio::test]
    async fn test_stats() {
        let queue = TaskQueue::new(3);
        queue.enqueue("t1", 100, "youtube").await;
        queue.enqueue("t2", 100, "playlist").await;
        queue.acquire("t1").await;

        let stats = queue.stats().await;
        assert_eq!(stats.running, 1);
        assert_eq!(stats.queued, 1);
        assert_eq!(stats.max_concurrent, 3);
    }
}
