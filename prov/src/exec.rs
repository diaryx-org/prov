//! A dependency-free executor for backends whose futures are already ready.
//!
//! [`crate::fs::Storage`] is async so genuinely async backends fit, but the
//! common native case — [`crate::fs::StdFs`] — produces futures that complete
//! on the first poll. [`block_on`] drives such a future without pulling in a
//! runtime: a no-op waker and a poll loop. It works for any future that makes
//! progress when polled (it busy-polls; it is *not* a fair scheduler), which
//! makes it suitable for CLIs and tests, not for I/O multiplexing.

use std::future::Future;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

/// Drive `future` to completion on the current thread by polling in a loop
/// with a no-op waker.
pub fn block_on<F: Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let waker = Waker::noop();
    let mut cx = Context::from_waker(waker);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => std::hint::spin_loop(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drives_a_ready_future() {
        assert_eq!(block_on(async { 7 }), 7);
    }

    #[test]
    fn drives_chained_storage_futures() {
        use crate::fs::{ReadStorage, StdFs};
        let exists = block_on(async {
            StdFs
                .try_exists(std::path::Path::new("/definitely/not/here"))
                .await
        });
        assert!(!exists.unwrap());
    }
}
