//! `std::cell::{Cell, RefCell}` for test fakes that must be `Sync`: the
//! legacy bridge makes an executor a `Mux` only when it can be shared
//! between threads.
#![allow(dead_code)]

use std::sync::{Mutex, MutexGuard};

pub(crate) struct Cell<T>(Mutex<T>);

impl<T: Copy> Cell<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }

    pub(crate) fn get(&self) -> T {
        *self.0.lock().unwrap()
    }

    pub(crate) fn set(&self, value: T) {
        *self.0.lock().unwrap() = value;
    }

    pub(crate) fn replace(&self, value: T) -> T {
        std::mem::replace(&mut self.0.lock().unwrap(), value)
    }
}

pub(crate) struct RefCell<T>(Mutex<T>);

impl<T> RefCell<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(Mutex::new(value))
    }

    pub(crate) fn borrow(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap()
    }

    pub(crate) fn borrow_mut(&self) -> MutexGuard<'_, T> {
        self.0.lock().unwrap()
    }
}
