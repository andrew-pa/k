use core::cell::OnceCell;

use crate::{fs::ByteStore, storage::BlockStore};
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

/// A RegistryHandler responds to requests to open registered resources by path.
#[async_trait]
pub trait RegistryHandler {
    /// Open a [BlockStore][crate::storage::BlockStore] resource at `subpath`.
    async fn open_block_store(&self, subpath: &Path) -> Result<Box<dyn BlockStore>, RegistryError>;
    /// Open a [ByteStore][crate::fs::ByteStore] resource at `subpath`.
    async fn open_byte_store(&self, subpath: &Path) -> Result<Box<dyn ByteStore>, RegistryError>;
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

/// The centeral resource registry, which resolves paths to resources.
pub struct Registry {
    root: Node,
}

impl Registry {
    fn new() -> Self {
        Registry {
            root: Node::Directory(HashMap::new()),
        }
    }

    /// Register a new handler to handle resource requests for all subpaths under `path`.
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

    fn find_handler<'s, 'p>(
        &'s self,
        p: &'p Path,
    ) -> Result<(&'p Path, &'s dyn RegistryHandler), RegistryError> {
        let mut pc = p.components();
        match pc.next() {
            Some(Component::Root) => {}
            _ => return Err(RegistryError::InvalidPath),
        }
        self.root.find(pc)
    }

    /// Open a [BlockStore][crate::storage::BlockStore] resource located at `p` using the handler
    /// registered for that path, if present.
    pub async fn open_block_store(&self, p: &Path) -> Result<Box<dyn BlockStore>, RegistryError> {
        let (subpath, h) = self.find_handler(p)?;
        h.open_block_store(subpath).await
    }

    /// Open a [ByteStore][crate::fs::ByteStore] resource located at `p` using the handler
    /// registered for that path, if present.
    pub async fn open_byte_store(&self, p: &Path) -> Result<Box<dyn ByteStore>, RegistryError> {
        let (subpath, h) = self.find_handler(p)?;
        h.open_byte_store(subpath).await
    }
}

static mut REG: OnceCell<RwLock<Registry>> = OnceCell::new();

/// Initialize the central resource registry.
pub fn init_registry() {
    unsafe {
        REG.set(RwLock::new(Registry::new()))
            .ok()
            .expect("init registry");
    }
}

/// Obtain a read-only handle to the central resource registry.
pub fn registry() -> RwLockReadGuard<'static, Registry> {
    unsafe { REG.get().expect("registry initialized").read() }
}

/// Obtain a read-write handle to the central resource registry.
pub fn registry_mut() -> RwLockWriteGuard<'static, Registry> {
    unsafe { REG.get().expect("registry initialized").write() }
}
