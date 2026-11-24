//! Test-only injector roots; never change the process environment or Windows profile.

use std::cell::RefCell;
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::rc::Rc;

struct Roots {
    home: PathBuf,
    #[cfg(windows)]
    local_data: PathBuf,
}

thread_local! {
    static ROOTS: RefCell<Option<Roots>> = const { RefCell::new(None) };
}

/// A temporary override for this test thread, restored even on early return or panic.
#[must_use]
pub(super) struct Scoped {
    previous: Option<Roots>,
    _same_thread: PhantomData<Rc<()>>,
}

impl Scoped {
    pub(super) fn new(temporary: &Path) -> std::io::Result<Self> {
        let home = temporary.join("injector-home");
        std::fs::create_dir_all(&home)?;
        #[cfg(windows)]
        let local_data = {
            let path = home.join("AppData").join("Local");
            std::fs::create_dir_all(&path)?;
            path
        };
        let previous = ROOTS.with(|active| {
            active.replace(Some(Roots {
                home,
                #[cfg(windows)]
                local_data,
            }))
        });
        Ok(Self { previous, _same_thread: PhantomData })
    }
}

impl Drop for Scoped {
    fn drop(&mut self) {
        ROOTS.with(|active| {
            active.replace(self.previous.take());
        });
    }
}

pub(super) fn home_dir() -> Option<PathBuf> {
    ROOTS.with(|active| active.borrow().as_ref().map(|roots| roots.home.clone()))
}

#[cfg(windows)]
pub(super) fn local_data_dir() -> Option<PathBuf> {
    ROOTS.with(|active| active.borrow().as_ref().map(|roots| roots.local_data.clone()))
}
