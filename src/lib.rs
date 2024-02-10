//! Rayon is a powerful library for making operations faster using multiple threads, but it can rarely if ever 
//! be assumed that such operations will be instantaneous.  It is often useful, therefore, if one
//! needs to process e.g. an iterator with millions of items, to display a progress bar so the user
//! knows approximately how long they will have to wait.
//!
//! This crate provides a thin wrapper over any type implementing `ParallelIterator` that keeps
//! track of the number of items that have been processed and allows accessing that value from
//! another thread.
//!
//! ```
//! use rayon::prelude::*;
//! use std::sync::{Arc, Mutex};
//! use std::time::Duration;
//! use std::thread::sleep;
//! use rayon_progress::ProgressAdaptor;
//!
//! let it = ProgressAdaptor::new(0..1000); // the constructor calls into_par_iter()
//! // get a handle to the number of items processed
//! // calling `progress.get()` repeatedly will return increasing values as processing continues
//! let progress = it.items_processed();
//! // it.len() is available when the underlying iterator implements IndexedParallelIterator (e.g.
//! // Range, Vec etc.)
//! // in other cases your code will have to either display indefinite progress or make an educated guess
//! let total = it.len();
//! // example method of transferring the result back to the thread that asked for it.
//! // you could also use `tokio::sync::oneshot`, `std::sync::mpsc` etc., or simply have the progress bar
//! // happen in a separate thread that dies when it gets notified that processing is complete.
//! let result = Arc::new(Mutex::new(None::<u32>));
//! 
//! // note that we wrap the iterator in a `ProgressAdaptor` *before* chaining any processing steps
//! // this is important for the count to be accurate, especially if later processing steps return
//! // fewer items (e.g. filter())
//! rayon::spawn({
//!     let result = result.clone();
//!     move || {
//!         let sum = it.map(|i| {
//!                sleep(Duration::from_millis(10)); // simulate some nontrivial computation
//!                i
//!             })
//!             .sum();
//!         *result.lock().unwrap() = Some(sum);
//!     }
//! });
//!
//! while result.lock().unwrap().is_none() {
//!     let percent = (progress.get() * 100) / total;
//!     println!("Processing... {}% complete", percent);
//! }
//! if let Some(result) = result.lock().unwrap().take() {
//!     println!("Done! Result was: {}", result);
//! };
//!
//! ```
use rayon::iter::{ParallelIterator, plumbing::{UnindexedConsumer, Consumer, Folder, Producer, ProducerCallback}, IndexedParallelIterator, IntoParallelIterator};
use std::sync::{Arc,atomic::{AtomicUsize, Ordering}};

/// A wrapper around a ParallelIterator that allows you to check, from another thread, how many
/// items have been processed at any given time, allowing, for example, displaying a progress bar
/// during a long-running Rayon operation.
pub struct ProgressAdaptor<I> {
    inner: I,
    items_processed: Arc<AtomicUsize>,
}

#[derive(Clone)]
pub struct ItemsProcessed(Arc<AtomicUsize>);

struct ProgressConsumer<C> {
    inner: C,
    items_processed: Arc<AtomicUsize>,
}

struct ProgressFolder<F> {
    inner: F,
    items_processed: Arc<AtomicUsize>,
}

struct ProgressProducer<P> {
    inner: P,
    items_processed: Arc<AtomicUsize>,
}

struct ProgressIterator<I> {
    inner: I,
    items_processed: Arc<AtomicUsize>,
}

impl ProgressAdaptor<()> {
    pub fn new<T>(iter: T) -> ProgressAdaptor<T::Iter> where T: IntoParallelIterator {
        ProgressAdaptor {
            inner: iter.into_par_iter(),
            items_processed: Arc::new(AtomicUsize::new(0)),
        }
    }
}

impl<T> ProgressAdaptor<T> {
    /// Returns a cheap-to-clone handle that can be used to get the number of items processed so
    /// far.  This method does not take a snapshot -- the value returned by the returned handle's
    /// `get()` method will update as processing continues.
    pub fn items_processed(&self) -> ItemsProcessed {
        ItemsProcessed(self.items_processed.clone())
    }
}

impl ItemsProcessed {
    pub fn get(&self) -> usize {
        self.0.load(Ordering::Relaxed)
    }
}

impl<I> ParallelIterator for ProgressAdaptor<I> where I: ParallelIterator {
    type Item=I::Item;

    fn drive_unindexed<C>(self, consumer: C) -> C::Result
    where
        C: UnindexedConsumer<Self::Item> {
        self.inner.drive_unindexed(ProgressConsumer {inner: consumer, items_processed: self.items_processed})
    }
}

impl<I> IndexedParallelIterator for ProgressAdaptor<I> where I: IndexedParallelIterator {
    fn len(&self) -> usize {
        self.inner.len()
    }

    fn drive<C: Consumer<Self::Item>>(self, consumer: C) -> C::Result {
        self.inner.drive(ProgressConsumer {inner: consumer, items_processed: self.items_processed})
    }

    fn with_producer<CB: ProducerCallback<Self::Item>>(self, callback: CB) -> CB::Output {
        struct ProgressCB<CB> {
            inner: CB,
            items_processed: Arc<AtomicUsize>,
        }
        impl<CB,T> ProducerCallback<T> for ProgressCB<CB> where CB: ProducerCallback<T> {
            type Output=CB::Output;

            fn callback<P>(self, producer: P) -> Self::Output where P: Producer<Item = T> {
                self.inner.callback(ProgressProducer{inner: producer, items_processed: self.items_processed})
            }
        }
        
        self.inner.with_producer(ProgressCB{inner: callback, items_processed: self.items_processed})
    }
}

impl<C,I> UnindexedConsumer<I> for ProgressConsumer<C> where C: UnindexedConsumer<I> {
    fn split_off_left(&self) -> Self {
        Self {inner: self.inner.split_off_left(), items_processed: self.items_processed.clone()}
    }

    fn to_reducer(&self) -> Self::Reducer {
        self.inner.to_reducer()
    }
}

impl<C,I> Consumer<I> for ProgressConsumer<C> where C: Consumer<I> {
    type Folder = ProgressFolder<C::Folder>;

    type Reducer = C::Reducer;

    type Result = C::Result;

    fn split_at(self, index: usize) -> (Self, Self, Self::Reducer) {
        let ProgressConsumer {inner, items_processed: entries_processed} = self;
        let (left, right, reducer) = inner.split_at(index);
        (ProgressConsumer{inner:left, items_processed: entries_processed.clone()},
         ProgressConsumer{inner:right, items_processed: entries_processed},
         reducer)
    }

    fn into_folder(self) -> Self::Folder {
        ProgressFolder {inner: self.inner.into_folder(), items_processed: self.items_processed}
    }

    fn full(&self) -> bool {
        self.inner.full()
    }
}

impl<F,I> Folder<I> for ProgressFolder<F> where F: Folder<I> {
    type Result=F::Result;

    fn consume(self, item: I) -> Self {
        let Self{inner, items_processed} = self;
        let inner = inner.consume(item);
        items_processed.fetch_add(1, Ordering::Relaxed);
        Self {inner, items_processed}
    }

    fn complete(self) -> Self::Result {
        self.inner.complete()
    }

    fn full(&self) -> bool {
        self.inner.full()
    }

    // the more I think about it, the more I realize the optimization with consume_iter() is
    // unsound and will produce wrong results.  best case scenario the count gets updated far too
    // early.  worst case the consumer realizes it is full midway through the iterator and we 
    // report more items were processed than actually were.
    // so i have left it out.
}

impl<P> Producer for ProgressProducer<P> where P: Producer {
    type Item=P::Item;

    type IntoIter=ProgressIterator<P::IntoIter>;

    fn into_iter(self) -> Self::IntoIter {
        ProgressIterator{inner: self.inner.into_iter(), items_processed: self.items_processed}
    }

    fn split_at(self, index: usize) -> (Self, Self) {
        let Self{inner, items_processed}=self;
        let (left,right) = inner.split_at(index);
        (Self{inner: left, items_processed: items_processed.clone()}, Self{inner: right, items_processed})
    }

    fn min_len(&self) -> usize {
        self.inner.min_len()
    }

    fn max_len(&self) -> usize {
        self.inner.max_len()
    }

    fn fold_with<F>(self, folder: F) -> F
    where
        F: Folder<Self::Item>,
    {
        self.inner.fold_with(ProgressFolder{inner: folder, items_processed: self.items_processed}).inner
    }
}

impl<I> Iterator for ProgressIterator<I> where I: Iterator {
    type Item=I::Item;
    fn next(&mut self) -> Option<Self::Item> {
        let res = self.inner.next();
        if res.is_some() {
            self.items_processed.fetch_add(1, Ordering::Relaxed);
        }
        res
    }
    fn size_hint(&self) -> (usize, Option<usize>) {
        self.inner.size_hint()
    }
}

impl<I> DoubleEndedIterator for ProgressIterator<I> where I: DoubleEndedIterator {
    fn next_back(&mut self) -> Option<Self::Item> {
        let res = self.inner.next_back();
        if res.is_some() {
            self.items_processed.fetch_add(1, Ordering::Relaxed);
        }
        res
    }
}

impl<I> ExactSizeIterator for ProgressIterator<I> where I: ExactSizeIterator {
    fn len(&self) -> usize {
        self.inner.len()
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use super::*;
    
    #[test]
    fn test() {
        let flag = Arc::new(AtomicBool::new(false));
        let flag2= flag.clone();
        let iter = ProgressAdaptor::new(0..1000);

        let items_processed = iter.items_processed();


        rayon::spawn(move || {
            while items_processed.get() < 500 {
                std::hint::spin_loop();
            }
            flag2.store(true, Ordering::Release);
        });

        let sum: u64 = iter.map(|x| {
            if x >= 500 {
                while !flag.load(Ordering::Acquire) {
                    std::hint::spin_loop();
                }
            }
            x
        }).sum();
        assert_eq!(sum, 499500);
        
    }
    
}
