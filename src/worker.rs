use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use crate::domain::{WorkRequest, WorkerError, WorkerId, WorkerOutput};

pub type WorkerFuture =
    Pin<Box<dyn Future<Output = Result<WorkerOutput, WorkerError>> + Send + 'static>>;

pub trait Worker: Send + Sync {
    fn id(&self) -> WorkerId;
    fn execute(&self, request: WorkRequest) -> WorkerFuture;
}

#[derive(Clone, Debug)]
pub struct SuccessfulWorker {
    id: WorkerId,
    output: WorkerOutput,
}

impl SuccessfulWorker {
    pub fn new(id: impl Into<String>, output: WorkerOutput) -> Self {
        Self {
            id: WorkerId::new(id),
            output,
        }
    }
}

impl Worker for SuccessfulWorker {
    fn id(&self) -> WorkerId {
        self.id.clone()
    }

    fn execute(&self, _request: WorkRequest) -> WorkerFuture {
        let output = self.output.clone();
        Box::pin(async move { Ok(output) })
    }
}

#[derive(Clone, Debug)]
pub struct ErrorWorker {
    id: WorkerId,
    message: String,
}

impl ErrorWorker {
    pub fn new(id: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            id: WorkerId::new(id),
            message: message.into(),
        }
    }
}

impl Worker for ErrorWorker {
    fn id(&self) -> WorkerId {
        self.id.clone()
    }

    fn execute(&self, _request: WorkRequest) -> WorkerFuture {
        let message = self.message.clone();
        Box::pin(async move { Err(WorkerError::Execution { message }) })
    }
}

#[derive(Clone, Debug)]
pub struct PanicWorker {
    id: WorkerId,
}

impl PanicWorker {
    pub fn new(id: impl Into<String>) -> Self {
        Self {
            id: WorkerId::new(id),
        }
    }
}

impl Worker for PanicWorker {
    fn id(&self) -> WorkerId {
        self.id.clone()
    }

    fn execute(&self, _request: WorkRequest) -> WorkerFuture {
        Box::pin(async move {
            panic!("controlled worker panic");
        })
    }
}

/// Composes a controlled late return around any real worker implementation.
///
/// The inner worker genuinely executes first. Its outcome is then withheld for
/// `delay`, allowing Meld's normal lease deadline to win without special-casing
/// the supervisor. This can later wrap a Rig-backed worker unchanged.
#[derive(Clone, Debug)]
pub struct ControlledDelayWorker<W> {
    inner: W,
    delay: Duration,
}

impl<W> ControlledDelayWorker<W> {
    pub fn new(inner: W, delay: Duration) -> Self {
        Self { inner, delay }
    }
}

impl<W> Worker for ControlledDelayWorker<W>
where
    W: Worker + 'static,
{
    fn id(&self) -> WorkerId {
        self.inner.id()
    }

    fn execute(&self, request: WorkRequest) -> WorkerFuture {
        let inner = self.inner.execute(request);
        let delay = self.delay;

        Box::pin(async move {
            let result = inner.await;
            tokio::time::sleep(delay).await;
            result
        })
    }
}
