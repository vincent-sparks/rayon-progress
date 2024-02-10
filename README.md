# rayon_progress

Rayon is a powerful library for making operations faster using multiple threads, but it can rarely if ever
be assumed that such operations will be instantaneous.  It is often useful, therefore, if one
needs to process e.g. an iterator with millions of items, to display a progress bar so the user
knows approximately how long they will have to wait.

This crate provides a thin wrapper over any type implementing `ParallelIterator` that keeps
track of the number of items that have been processed and allows accessing that value from
another thread.

```rust
use rayon::prelude::*;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::thread::sleep;
use rayon_progress::ProgressAdaptor;

let it = ProgressAdaptor::new(0..1000); // the constructor calls into_par_iter()
// get a handle to the number of items processed
// calling `progress.get()` repeatedly will return increasing values as processing continues
let progress = it.items_processed();
// it.len() is available when the underlying iterator implements IndexedParallelIterator (e.g.
// Range, Vec etc.)
// in other cases your code will have to either display indefinite progress or make an educated guess
let total = it.len();
// example method of transferring the result back to the thread that asked for it.
// you could also use `tokio::sync::oneshot`, `std::sync::mpsc` etc., or simply have the progress bar
// happen in a separate thread that dies when it gets notified that processing is complete.
let result = Arc::new(Mutex::new(None::<u32>));

// note that we wrap the iterator in a `ProgressAdaptor` *before* chaining any processing steps
// this is important for the count to be accurate, especially if later processing steps return
// fewer items (e.g. filter())
rayon::spawn({
    let result = result.clone();
    move || {
        let sum = it.map(|i| {
               sleep(Duration::from_millis(10)); // simulate some nontrivial computation
               i
            })
            .sum();
        *result.lock().unwrap() = Some(sum);
    }
});

while result.lock().unwrap().is_none() {
    let percent = (progress.get() * 100) / total;
    println!("Processing... {}% complete", percent);
}
if let Some(result) = result.lock().unwrap().take() {
    println!("Done! Result was: {}", result);
};

```
