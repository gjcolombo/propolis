// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.

//! Traits for working with devices that handle input and output one byte at a
//! time (e.g. through a guest I/O port).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

pub mod console;
mod file_out;
pub mod history_buffer;
pub mod pollers;
mod sock;

pub use file_out::BlockingFileOutput;
pub use sock::UDSock;

/// A callback function that's invoked to notify the callee that a byte sink is
/// now accepting new writes. See [`Sink::set_notifier`].
pub type SinkNotifier = Box<dyn Fn(&dyn Sink) + Send + Sync + 'static>;

/// A callback function that's invoked to notify the callee that a byte source
/// has new data to read. See [`Source::set_notifier`].
pub type SourceNotifier = Box<dyn Fn(&dyn Source) + Send + Sync + 'static>;

/// A callback function that synchronously processes incoming bytes from a
/// device. See [`BlockingSource::set_consumer`].
pub type BlockingSourceConsumer = Box<dyn Fn(&[u8]) + Send + Sync + 'static>;

/// Represents a device to which bytes can be written.
pub trait Sink: Send + Sync + 'static {
    /// Attempts to write `data` to the device. Returns true if the byte was
    /// written and false otherwise.
    // XXX: make this slice based
    fn write(&self, data: u8) -> bool;

    /// Sets a notifier callback that will be called when the sink becomes
    /// writable.
    ///
    /// The callback may be called on another thread before this function
    /// returns, so the caller must not hold any locks the callback requires.
    fn set_notifier(&self, f: Option<SinkNotifier>);
}

/// Represents a device which can output bytes to be read.
pub trait Source: Send + Sync + 'static {
    /// Attempts to read a byte from the device, returning `Some` if a byte is
    /// available and `None` otherwise.
    // XXX: make this slice based
    fn read(&self) -> Option<u8>;

    /// Attempts to read and discard `count` bytes from this byte source.
    /// Returns the number of bytes that were actually discarded.
    fn discard(&self, count: usize) -> usize;

    /// Sets the auto-discard discipline for this byte source: if enabled, bytes
    /// produced by this source are automatically dropped before they are made
    /// available to the `read` function.
    ///
    /// This does not apply retroactively: if a byte is available to be read
    /// when auto-discard is enabled, the available byte is not discarded.
    fn set_autodiscard(&self, active: bool);

    /// Sets a notifier callback that will be called when the source becomes
    /// readable.
    ///
    /// The callback may be called on another thread before this function
    /// returns, so the caller must not hold any locks the callback requires.
    fn set_notifier(&self, f: Option<SourceNotifier>);
}

/// Represents a device that produces bytes to be processed by a registrant as
/// soon as they become available (as opposed to a [`Source`], where registrants
/// are notified when bytes become available and then need to call back into the
/// source to read them).
pub trait BlockingSource: Send + Sync + 'static {
    /// Sets the callback to which incoming bytes from this source should be
    /// sent for processing.
    fn set_consumer(&self, f: Option<BlockingSourceConsumer>);
}

type NotifierFn<T> = dyn Fn(&T) + Send + Sync + 'static;

/// Manages notification functions for [`Source`]s and [`Sink`]s.
pub struct NotifierCell<T: ?Sized> {
    /// True if the inner `notifier` contains a callback.
    is_set: AtomicBool,

    /// The currently-registered callback.
    notifier: Mutex<Option<Box<NotifierFn<T>>>>,
}

impl<T: ?Sized> NotifierCell<T> {
    pub fn new() -> Self {
        Self { is_set: AtomicBool::new(false), notifier: Mutex::new(None) }
    }
}

impl NotifierCell<dyn Sink> {
    pub fn set(&self, f: Option<SinkNotifier>) {
        let mut guard = self.notifier.lock().unwrap();
        self.is_set.store(f.is_some(), Ordering::Release);
        *guard = f;
    }
    pub fn notify(&self, sink: &dyn Sink) {
        if self.is_set.load(Ordering::Acquire) {
            let guard = self.notifier.lock().unwrap();
            if let Some(f) = guard.as_ref() {
                f(sink);
            }
        }
    }
}
impl NotifierCell<dyn Source> {
    pub fn set(&self, f: Option<SourceNotifier>) {
        let mut guard = self.notifier.lock().unwrap();
        self.is_set.store(f.is_some(), Ordering::Release);
        *guard = f;
    }
    pub fn notify(&self, source: &dyn Source) {
        if self.is_set.load(Ordering::Acquire) {
            let guard = self.notifier.lock().unwrap();
            if let Some(f) = guard.as_ref() {
                f(source);
            }
        }
    }
}

/// Manages notification functions for a [`BlockingSource`].
pub struct ConsumerCell {
    is_set: AtomicBool,
    consumer: Mutex<Option<BlockingSourceConsumer>>,
}
impl ConsumerCell {
    pub fn new() -> Self {
        Self { is_set: AtomicBool::new(false), consumer: Mutex::new(None) }
    }
    pub fn set(&self, f: Option<BlockingSourceConsumer>) {
        let mut guard = self.consumer.lock().unwrap();
        self.is_set.store(f.is_some(), Ordering::Release);
        *guard = f;
    }
    pub fn consume(&self, data: &[u8]) {
        if self.is_set.load(Ordering::Acquire) {
            let guard = self.consumer.lock().unwrap();
            if let Some(f) = guard.as_ref() {
                f(data);
            }
        }
    }
}
