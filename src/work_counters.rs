use std::cell::Cell;

#[derive(Clone, Copy, Debug, Default, serde::Serialize)]
pub(crate) struct Counts {
    pub writes: u64,
    pub drains: u64,
    pub drain_errors: u64,
}
thread_local! {
    static ACTIVE: Cell<Option<Counts>> = const { Cell::new(None) };
}
pub(crate) struct Scope;
impl Scope {
    pub fn start() -> Self {
        ACTIVE.with(|active| {
            assert!(active.get().is_none());
            active.set(Some(Counts::default()));
        });
        Self
    }
    pub fn finish(self) -> Counts {
        ACTIVE.with(|active| active.take().unwrap())
    }
}
impl Drop for Scope {
    fn drop(&mut self) {
        ACTIVE.with(|active| active.set(None));
    }
}
pub(crate) fn write_begin() {
    ACTIVE.with(|active| {
        if let Some(mut counts) = active.get() {
            counts.writes += 1;
            active.set(Some(counts));
        }
    });
}
pub(crate) fn drain() {
    ACTIVE.with(|active| {
        if let Some(mut counts) = active.get() {
            counts.drains += 1;
            active.set(Some(counts));
        }
    });
}

pub(crate) fn drain_result(result: &Result<crate::DrainReport, String>) {
    ACTIVE.with(|active| {
        if let Some(mut counts) = active.get() {
            counts.drain_errors += u64::from(result.is_err());
            active.set(Some(counts));
        }
    });
}
