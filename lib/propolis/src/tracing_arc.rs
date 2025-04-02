use std::ops::{Deref, DerefMut};
use std::sync::Arc;

pub struct TracingArc<T>(pub Arc<T>);

#[usdt::provider(provider = "propolis")]
mod probes {
    fn arc_acquire(addr: u64, new_count: u64) {}
    fn arc_release(addr: u64, new_count: u64) {}
}

impl<T> TracingArc<T> {
    pub fn new(data: T) -> Self {
        let this = Arc::new(data);
        probes::arc_acquire!(|| (std::sync::Arc::as_ptr(&this) as u64, 1u64));
        Self(this)
    }
}

impl<T> Clone for TracingArc<T> {
    fn clone(&self) -> Self {
        let count = std::sync::Arc::strong_count(&self.0) as u64;
        probes::arc_acquire!(|| (
            std::sync::Arc::as_ptr(&self.0) as u64,
            count + 1
        ));
        Self(self.0.clone())
    }
}

impl<T> Drop for TracingArc<T> {
    fn drop(&mut self) {
        let count = std::sync::Arc::strong_count(&self.0) as u64;
        probes::arc_release!(|| (
            std::sync::Arc::as_ptr(&self.0) as u64,
            count - 1
        ));
    }
}

impl<T> Deref for TracingArc<T> {
    type Target = Arc<T>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> DerefMut for TracingArc<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
