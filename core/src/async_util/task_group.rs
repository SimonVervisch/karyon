use std::{future::Future, sync::Arc, sync::Mutex};

use async_task::FallibleTask;

use super::{executor::global_executor, select, CondWait, Either, Executor};

/// TaskGroup A group that contains spawned tasks.
///
/// # Example
///
/// ```
///
/// use std::sync::Arc;
///
/// use karyon_core::async_util::TaskGroup;
///
/// async {
///
///     let ex = Arc::new(smol::Executor::new());
///     let group = TaskGroup::with_executor(ex);
///
///     group.spawn(smol::Timer::never(), |_| async {});
///
///     group.cancel().await;
///
/// };
///
/// ```
///
pub struct TaskGroup<'a> {
    tasks: Mutex<Vec<TaskHandler>>,
    stop_signal: Arc<CondWait>,
    executor: Executor<'a>,
}

impl TaskGroup<'static> {
    /// Creates a new TaskGroup without providing an executor
    ///
    /// This will spawn a task onto a global executor (single-threaded by default).
    pub fn new() -> Self {
        Self {
            tasks: Mutex::new(Vec::new()),
            stop_signal: Arc::new(CondWait::new()),
            executor: global_executor(),
        }
    }
}

impl<'a> TaskGroup<'a> {
    /// Creates a new TaskGroup by providing an executor
    pub fn with_executor(executor: Executor<'a>) -> Self {
        Self {
            tasks: Mutex::new(Vec::new()),
            stop_signal: Arc::new(CondWait::new()),
            executor,
        }
    }

    /// Spawns a new task and calls the callback after it has completed
    /// or been canceled. The callback will have the `TaskResult` as a
    /// parameter, indicating whether the task completed or was canceled.
    pub fn spawn<T, Fut, CallbackF, CallbackFut>(&self, fut: Fut, callback: CallbackF)
    where
        T: Send + Sync + 'a,
        Fut: Future<Output = T> + Send + 'a,
        CallbackF: FnOnce(TaskResult<T>) -> CallbackFut + Send + 'a,
        CallbackFut: Future<Output = ()> + Send + 'a,
    {
        let task = TaskHandler::new(
            self.executor.clone(),
            fut,
            callback,
            self.stop_signal.clone(),
        );
        self.tasks.lock().unwrap().push(task);
    }

    /// Checks if the TaskGroup is empty.
    pub fn is_empty(&self) -> bool {
        self.tasks.lock().unwrap().is_empty()
    }

    /// Get the number of the tasks in the group.
    pub fn len(&self) -> usize {
        self.tasks.lock().unwrap().len()
    }

    /// Cancels all tasks in the group.
    pub async fn cancel(&self) {
        self.stop_signal.broadcast().await;

        loop {
            let task = self.tasks.lock().unwrap().pop();
            if let Some(t) = task {
                t.cancel().await
            } else {
                break;
            }
        }
    }
}

impl Default for TaskGroup<'static> {
    fn default() -> Self {
        Self::new()
    }
}

/// The result of a spawned task.
#[derive(Debug)]
pub enum TaskResult<T> {
    Completed(T),
    Cancelled,
}

impl<T: std::fmt::Debug> std::fmt::Display for TaskResult<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self {
            TaskResult::Cancelled => write!(f, "Task cancelled"),
            TaskResult::Completed(res) => write!(f, "Task completed: {:?}", res),
        }
    }
}

/// TaskHandler
pub struct TaskHandler {
    task: FallibleTask<()>,
    cancel_flag: Arc<CondWait>,
}

impl<'a> TaskHandler {
    /// Creates a new task handler
    fn new<T, Fut, CallbackF, CallbackFut>(
        ex: Executor<'a>,
        fut: Fut,
        callback: CallbackF,
        stop_signal: Arc<CondWait>,
    ) -> TaskHandler
    where
        T: Send + Sync + 'a,
        Fut: Future<Output = T> + Send + 'a,
        CallbackF: FnOnce(TaskResult<T>) -> CallbackFut + Send + 'a,
        CallbackFut: Future<Output = ()> + Send + 'a,
    {
        let cancel_flag = Arc::new(CondWait::new());
        let cancel_flag_c = cancel_flag.clone();
        let task = ex
            .spawn(async move {
                // Waits for either the stop signal or the task to complete.
                let result = select(stop_signal.wait(), fut).await;

                let result = match result {
                    Either::Left(_) => TaskResult::Cancelled,
                    Either::Right(res) => TaskResult::Completed(res),
                };

                // Call the callback
                callback(result).await;

                cancel_flag_c.signal().await;
            })
            .fallible();

        TaskHandler { task, cancel_flag }
    }

    /// Cancels the task.
    async fn cancel(self) {
        self.cancel_flag.wait().await;
        self.task.cancel().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{future, sync::Arc};

    #[test]
    fn test_task_group_with_executor() {
        let ex = Arc::new(smol::Executor::new());
        smol::block_on(ex.clone().run(async move {
            let group = Arc::new(TaskGroup::with_executor(ex));

            group.spawn(future::ready(0), |res| async move {
                assert!(matches!(res, TaskResult::Completed(0)));
            });

            group.spawn(future::pending::<()>(), |res| async move {
                assert!(matches!(res, TaskResult::Cancelled));
            });

            let groupc = group.clone();
            group.spawn(
                async move {
                    groupc.spawn(future::pending::<()>(), |res| async move {
                        assert!(matches!(res, TaskResult::Cancelled));
                    });
                },
                |res| async move {
                    assert!(matches!(res, TaskResult::Completed(_)));
                },
            );

            // Do something
            smol::Timer::after(std::time::Duration::from_millis(50)).await;
            group.cancel().await;
        }));
    }

    #[test]
    fn test_task_group() {
        smol::block_on(async {
            let group = Arc::new(TaskGroup::new());

            group.spawn(future::ready(0), |res| async move {
                assert!(matches!(res, TaskResult::Completed(0)));
            });

            group.spawn(future::pending::<()>(), |res| async move {
                assert!(matches!(res, TaskResult::Cancelled));
            });

            let groupc = group.clone();
            group.spawn(
                async move {
                    groupc.spawn(future::pending::<()>(), |res| async move {
                        assert!(matches!(res, TaskResult::Cancelled));
                    });
                },
                |res| async move {
                    assert!(matches!(res, TaskResult::Completed(_)));
                },
            );

            // Do something
            smol::Timer::after(std::time::Duration::from_millis(50)).await;
            group.cancel().await;
        });
    }
}
