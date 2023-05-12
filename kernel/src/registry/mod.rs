use core::cell::OnceCell;

use crate::io::BlockStore;
use alloc::{boxed::Box, vec::Vec};
use async_trait::async_trait;

pub mod path;
pub use path::{Path, PathBuf};
use spin::{RwLock, RwLockReadGuard, RwLockWriteGuard};

pub enum RegistryError {
    NotFound(PathBuf),
    Unsupported,
}

#[async_trait]
pub trait RegistryHandler {
    async fn open_block_store(&self, subpath: &Path) -> Result<Box<dyn BlockStore>, RegistryError>;
}

pub struct Registry {}

impl Registry {
    fn new() -> Self {
        Registry {}
    }

    pub fn register(&mut self, prefix: &Path, handler: Box<dyn RegistryHandler>) {
        todo!()
    }

    pub fn open_block_store(&self, p: &Path) -> Result<Box<dyn BlockStore>, RegistryError> {
        todo!()
    }
}

static mut REG: OnceCell<RwLock<Registry>> = OnceCell::new();

pub fn init_registry() {
    unsafe {
        REG.set(RwLock::new(Registry::new()))
            .ok()
            .expect("init registry");
    }
}

pub fn registry() -> RwLockReadGuard<'static, Registry> {
    unsafe { REG.get().unwrap().read() }
}

pub fn registry_mut() -> RwLockWriteGuard<'static, Registry> {
    unsafe { REG.get().unwrap().write() }
}
