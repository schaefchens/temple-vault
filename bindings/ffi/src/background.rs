//! Off the JS thread: natively, calls run on Temple's own thread, one after another in the
//! order they came. wasm has no threads, so they run at once – the web keeps them off its
//! main thread by hosting the whole module in a worker.

use crate::TempleError;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn run<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, TempleError> + Send + 'static,
) -> Result<T, TempleError> {
    use std::sync::{LazyLock, mpsc};

    type Job = Box<dyn FnOnce() + Send>;
    static JOBS: LazyLock<mpsc::Sender<Job>> = LazyLock::new(|| {
        let (jobs, queue) = mpsc::channel::<Job>();
        std::thread::Builder::new()
            .name("temple".into())
            .spawn(move || {
                for job in queue {
                    // A panic fails its own call – its sender drops – and the thread goes on.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                }
            })
            .expect("Temple's thread starts");
        jobs
    });

    let (done, result) = futures_channel::oneshot::channel();
    let job: Job = Box::new(move || {
        let _ = done.send(work());
    });
    JOBS.send(job).map_err(|_| TempleError::Io("Temple's thread has stopped".into()))?;
    result.await.map_err(|_| TempleError::Io("Temple panicked".into()))?
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn run<T>(work: impl FnOnce() -> Result<T, TempleError>) -> Result<T, TempleError> {
    work()
}
