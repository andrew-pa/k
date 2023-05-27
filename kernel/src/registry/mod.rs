use core::cell::OnceCell;

use crate::storage::BlockStore;
use alloc::{boxed::Box, string::String, vec::Vec};
use async_trait::async_trait;

pub mod path;
use hashbrown::HashMap;
pub use path::{Path, PathBuf};
use snafu::Snafu;
use spin::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use self::path::{Component, Components};

#[derive(Debug, Snafu)]
#[snafu(visibility(pub), module(error))]
pub enum RegistryError {
    NotFound { path: PathBuf },
    Unsupported,
    HandlerAlreadyRegistered { name: String },
    InvalidPath,
    Other { source: Box<dyn snafu::Error> },
}

#[async_trait]
pub trait RegistryHandler {
    async fn open_block_store(&self, subpath: &Path) -> Result<Box<dyn BlockStore>, RegistryError>;
}

// TODO: storing these as strings is bad as short strings might take up an unexpectedly large
// amount of memory due to heap block headers and alignment requirements
// maybe it would be better to intern them somehow?
enum Node {
    Directory(HashMap<String, Node>),
    Handler(Box<dyn RegistryHandler>),
}

impl Node {
    fn add(
        &mut self,
        mut prefix: Components,
        name: &str,
        handler: Box<dyn RegistryHandler>,
    ) -> Result<(), RegistryError> {
        log::trace!("{}.{name}", prefix.as_path());
        use Node::*;
        match (prefix.next(), self) {
            (Some(Component::Name(s)), Directory(children)) => children
                .entry(s.into())
                .or_insert(Directory(Default::default()))
                .add(prefix, name, handler),
            (None, Directory(children)) => {
                if children.contains_key(name) {
                    Err(RegistryError::HandlerAlreadyRegistered { name: name.into() })
                } else {
                    children.insert(name.into(), Handler(handler));
                    Ok(())
                }
            }
            (p @ Some(Component::Root | Component::ParentDir | Component::CurrentDir), _) => {
                panic!("unexpected path component encountered {p:?}")
            }
            _ => Err(RegistryError::HandlerAlreadyRegistered { name: name.into() }),
        }
    }

    fn find<'p>(
        &self,
        mut path: Components<'p>,
    ) -> Result<(&'p Path, &dyn RegistryHandler), RegistryError> {
        match (path.next(), self) {
            (Some(Component::Name(n)), Node::Directory(children)) => match children.get(n) {
                Some(Node::Handler(h)) => Ok((path.as_path(), h.as_ref())),
                Some(n) => n.find(path),
                None => Err(RegistryError::NotFound {
                    path: path.as_path().into(),
                }),
            },
            (p @ Some(Component::Root | Component::ParentDir | Component::CurrentDir), _) => {
                panic!("unexpected path component encountered {p:?}")
            }
            (None, Node::Directory(_)) => Err(todo!()),
            _ => todo!(),
        }
    }
}

pub struct Registry {
    root: Node,
}

impl Registry {
    fn new() -> Self {
        Registry {
            root: Node::Directory(HashMap::new()),
        }
    }

    pub fn register(
        &mut self,
        path: &Path,
        handler: Box<dyn RegistryHandler>,
    ) -> Result<(), RegistryError> {
        let mut pc = path.components();
        let name = pc
            .next_back()
            .and_then(|c| match c {
                Component::Name(s) => Some(s),
                _ => None,
            })
            .ok_or(RegistryError::InvalidPath)?;
        log::debug!("registering {path}");
        match pc.next() {
            Some(Component::Root) => self.root.add(pc, name, handler),
            _ => Err(RegistryError::InvalidPath),
        }
    }

    pub async fn open_block_store(&self, p: &Path) -> Result<Box<dyn BlockStore>, RegistryError> {
        let mut pc = p.components();
        match pc.next() {
            Some(Component::Root) => {}
            _ => return Err(RegistryError::InvalidPath),
        }
        let (subpath, h) = self.root.find(pc)?;
        h.open_block_store(subpath).await
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
