// SPDX-FileCopyrightText: 2022  Emmanuele Bassi
// SPDX-License-Identifier: GPL-3.0-or-later

use super::*;

// Runs `f`, which reads a file that anyone could have crafted, and returns
// `fallback()` if it panics. The tag reader can panic on a malformed file,
// and without this one file would stop a whole load or scan, every time
pub(crate) fn guarded<T>(
    what: impl std::fmt::Display,
    f: impl FnOnce() -> T,
    fallback: impl FnOnce() -> T,
) -> T {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| {
        warn!("Skipping {what}, which couldn't be read");
        fallback()
    })
}

// How many files are read at a time where reading them mostly waits, as
// decoding a cover waits for its sandboxed loader: a few at a time go
// faster, even on fewer cores
pub(crate) const READING_THREADS: usize = 4;

// Runs `work` on each of the `items` in up to `threads` threads, and hands
// each result to `each` on this thread as it comes, with the position of
// its item, in no particular order. It returns once every item is done, or
// with the first error of `each`, once the threads have finished the items
// they were on
pub(crate) fn in_threads<T: Sync, R: Send, E>(
    items: &[T],
    threads: usize,
    work: impl Fn(&T) -> R + Sync,
    mut each: impl FnMut(usize, R) -> Result<(), E>,
) -> Result<(), E> {
    use std::sync::atomic::{AtomicUsize, Ordering};

    let next = AtomicUsize::new(0);
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        for _ in 0..threads.min(items.len()) {
            let sender = sender.clone();
            let (next, work) = (&next, &work);
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some(item) = items.get(index) else {
                        break;
                    };
                    // The receiver is gone once `each` failed
                    if sender.send((index, work(item))).is_err() {
                        break;
                    }
                }
            });
        }
        drop(sender);
        for (index, result) in receiver {
            each(index, result)?;
        }
        Ok(())
    })
}
