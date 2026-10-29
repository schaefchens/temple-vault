//! Off the JS thread: natively, calls run on Temple's own thread, one after another in the
//! order they came – a lock first – and the stack they used is wiped after each. wasm has no
//! threads, so they run at once – the web keeps them off its main thread by hosting the whole
//! module in a worker.

use crate::TempleError;

#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn run<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, TempleError> + Send + 'static,
) -> Result<T, TempleError> {
    queued(work, false).await
}

/// Before every call still waiting: for a lock.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) async fn run_first<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, TempleError> + Send + 'static,
) -> Result<T, TempleError> {
    // iOS suspends an app moments after it moves to the background: a lock gets time to finish.
    #[cfg(target_os = "ios")]
    let activity = time::Activity::begin("Temple locks a space");
    let done = queued(work, true).await;
    #[cfg(target_os = "ios")]
    drop(activity);
    done
}

#[cfg(not(target_arch = "wasm32"))]
async fn queued<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, TempleError> + Send + 'static,
    first: bool,
) -> Result<T, TempleError> {
    use std::collections::VecDeque;
    use std::sync::{Condvar, LazyLock, Mutex, PoisonError};

    type Job = Box<dyn FnOnce() + Send>;
    struct Queue {
        jobs: Mutex<VecDeque<Job>>,
        waiting: Condvar,
    }
    static QUEUE: LazyLock<&'static Queue> = LazyLock::new(|| {
        let queue: &'static Queue = Box::leak(Box::new(Queue { jobs: Mutex::default(), waiting: Condvar::new() }));
        std::thread::Builder::new()
            .name("temple".into())
            .spawn(move || {
                loop {
                    let job = {
                        let mut jobs = queue.jobs.lock().unwrap_or_else(PoisonError::into_inner);
                        loop {
                            match jobs.pop_front() {
                                Some(job) => break job,
                                None => jobs = queue.waiting.wait(jobs).unwrap_or_else(PoisonError::into_inner),
                            }
                        }
                    };
                    // A panic fails its own call – its sender drops – and the thread goes on.
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
                    temple_vault::wipe_stack();
                }
            })
            .expect("Temple's thread starts");
        queue
    });

    let (done, result) = futures_channel::oneshot::channel();
    let job: Job = Box::new(move || {
        let _ = done.send(work());
    });
    {
        let mut jobs = QUEUE.jobs.lock().unwrap_or_else(PoisonError::into_inner);
        if first { jobs.push_front(job) } else { jobs.push_back(job) }
    }
    QUEUE.waiting.notify_one();
    result.await.map_err(|_| TempleError::Io("Temple panicked".into()))?
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn run<T>(work: impl FnOnce() -> Result<T, TempleError>) -> Result<T, TempleError> {
    let done = work();
    temple_vault::wipe_stack();
    done
}

#[cfg(target_arch = "wasm32")]
pub(crate) async fn run_first<T>(work: impl FnOnce() -> Result<T, TempleError>) -> Result<T, TempleError> {
    run(work).await
}

/// Background time on iOS: an expiring activity, whose block keeps the app from being
/// suspended until the work is done – or until iOS takes the time back.
#[cfg(target_os = "ios")]
mod time {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSProcessInfo, NSString};
    use std::sync::{Arc, Condvar, Mutex, PoisonError};

    /// Holds the app awake until dropped.
    pub(crate) struct Activity(Arc<(Mutex<bool>, Condvar)>);

    impl Activity {
        pub(crate) fn begin(reason: &str) -> Activity {
            let done = Arc::new((Mutex::new(false), Condvar::new()));
            let waiting = done.clone();
            // Called on a queue of the system's: with `expired` false while the app may go on –
            // it stays awake until this returns – and again with true once its time is up.
            let block = RcBlock::new(move |expired: Bool| {
                let (finished, changed) = &*waiting;
                let mut finished = finished.lock().unwrap_or_else(PoisonError::into_inner);
                if expired.as_bool() {
                    *finished = true;
                    changed.notify_all();
                    return;
                }
                while !*finished {
                    finished = changed.wait(finished).unwrap_or_else(PoisonError::into_inner);
                }
            });
            let reason = NSString::from_str(reason);
            // SAFETY: the block takes what Foundation hands it, a BOOL, and owns what it uses.
            unsafe { NSProcessInfo::processInfo().performExpiringActivityWithReason_usingBlock(&reason, &block) };
            Activity(done)
        }
    }

    impl Drop for Activity {
        fn drop(&mut self) {
            let (finished, changed) = &*self.0;
            *finished.lock().unwrap_or_else(PoisonError::into_inner) = true;
            changed.notify_all();
        }
    }
}
