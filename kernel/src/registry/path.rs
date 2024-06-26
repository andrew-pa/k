use core::{borrow::Borrow, fmt::Display, ops::Deref};

use alloc::string::String;

const PATH_SEP: char = '/';

#[derive(Debug, Eq, Hash, PartialEq)]
pub enum Component<'p> {
    Root,
    CurrentDir,
    ParentDir,
    Name(&'p str),
}

impl<'p> Component<'p> {
    // prevent this from being public by not implementing From
    fn from_str(s: &'p str) -> Self {
        match s {
            "." => Component::CurrentDir,
            ".." => Component::ParentDir,
            _ => Component::Name(s),
        }
    }
}

pub struct Components<'p> {
    s: &'p str,
    front: usize,
    back: usize,
}

impl<'p> Components<'p> {
    fn new(s: &'p str) -> Self {
        Components {
            s,
            front: 0,
            back: s.len(),
        }
    }

    pub fn as_path(&self) -> &'p Path {
        Path::new(&self.s[self.front..self.back])
    }
}

impl<'p> Iterator for Components<'p> {
    type Item = Component<'p>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.back <= self.front {
            return None;
        }
        let cur = &self.s[self.front..self.back];
        if let Some(next_sep) = cur.find(PATH_SEP) {
            if next_sep == 0 {
                self.front += PATH_SEP.len_utf8();
                Some(Component::Root)
            } else {
                self.front += next_sep + PATH_SEP.len_utf8();
                Some(Component::from_str(&cur[0..next_sep]))
            }
        } else {
            self.front = self.back;
            Some(Component::from_str(cur))
        }
    }
}

impl<'p> DoubleEndedIterator for Components<'p> {
    fn next_back(&mut self) -> Option<Self::Item> {
        if self.back <= self.front {
            return None;
        }
        let mut cur = &self.s[self.front..self.back];
        if cur == "/" {
            self.back = self.front;
            return Some(Component::Root);
        }
        while cur.rfind(PATH_SEP).is_some_and(|i| i == self.back - 1) {
            self.back -= 1;
            if self.back <= self.front {
                return None;
            }
            cur = &cur[self.front..self.back];
        }
        if let Some(last_sep) = cur.rfind(PATH_SEP) {
            self.back -= cur.len() - last_sep - 1;
            Some(Component::from_str(&cur[last_sep + 1..]))
        } else {
            self.back = self.front;
            Some(Component::from_str(cur))
        }
    }
}

#[derive(Debug, Eq, PartialEq, Hash)]
pub struct Path {
    s: str,
}

impl Path {
    pub fn new(s: &str) -> &Path {
        unsafe { &*(s as *const str as *const Path) }
    }

    pub fn len(&self) -> usize {
        self.s.len()
    }

    pub fn is_empty(&self) -> bool {
        self.s.is_empty()
    }

    pub fn is_absolute(&self) -> bool {
        self.components()
            .next()
            .is_some_and(|c| c == Component::Root)
    }

    pub fn components(&self) -> Components {
        Components::new(&self.s)
    }

    pub fn parent(&self) -> Option<&Path> {
        let mut comps = self.components();
        comps.next_back().and_then(move |p| match p {
            Component::Name(_) | Component::CurrentDir | Component::ParentDir => {
                Some(comps.as_path())
            }
            _ => None,
        })
    }

    pub fn file_name(&self) -> Option<&str> {
        let mut comps = self.components();
        comps.next_back().and_then(move |p| match p {
            Component::Root => None,
            // this is what the std::path::Path does?
            Component::CurrentDir => comps.as_path().file_name(),
            Component::ParentDir => None,
            Component::Name(s) => Some(s),
        })
    }

    pub fn as_str(&self) -> &str {
        &self.s
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<&Self, core::str::Utf8Error> {
        core::str::from_utf8(bytes).map(Self::new)
    }
}

impl Display for Path {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", &self.s)
    }
}

impl AsRef<Path> for &str {
    fn as_ref(&self) -> &Path {
        Path::new(self)
    }
}

impl AsRef<Path> for Path {
    fn as_ref(&self) -> &Path {
        self
    }
}

#[derive(Debug)]
pub struct PathBuf {
    s: String,
}

impl<S: AsRef<str>> From<S> for PathBuf {
    fn from(value: S) -> Self {
        PathBuf {
            s: String::from(value.as_ref()),
        }
    }
}

impl From<&Path> for PathBuf {
    fn from(value: &Path) -> Self {
        PathBuf::from(value.as_str())
    }
}

impl Borrow<Path> for PathBuf {
    fn borrow(&self) -> &Path {
        Path::new(&self.s)
    }
}

impl Deref for PathBuf {
    type Target = Path;

    fn deref(&self) -> &Self::Target {
        self.borrow()
    }
}

impl Display for PathBuf {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "{}", &self.s)
    }
}

impl PathBuf {
    pub fn push(&mut self, p: impl AsRef<Path>) {
        if !self.s.ends_with(PATH_SEP) {
            self.s.push(PATH_SEP);
        }
        self.s.push_str(p.as_ref().as_str());
    }

    pub fn pop(&mut self) -> bool {
        match self.parent().map(|p| p.len()) {
            Some(new_len) => {
                self.s.truncate(new_len);
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use paste::paste;

    macro_rules! path_comp_test {
        ($name : ident, $path : literal = $($comps:expr),+) => {
            paste!{
                #[test_case]
                fn [<components_fwd_ $name>]() {
                    use Component::*;
                    let binding = Path::new($path);
                    let mut c = binding.components();
                    $(
                        assert_eq!(c.next(), Some($comps));
                    )*
                    assert_eq!(c.next(), None);
                }
                #[test_case]
                fn [<components_bkwd_ $name>]() {
                    use Component::*;
                    let binding = Path::new($path);
                    let mut c = binding.components();
                    for ec in [ $($comps, )* ].iter().rev() {
                        assert_eq!(c.next_back().as_ref(), Some(ec));
                    }
                    assert_eq!(c.next_back(), None);
                }
            }
        };
    }

    path_comp_test!(root, "/" = Root);
    path_comp_test!(cur_dir, "." = CurrentDir);
    path_comp_test!(par_dir, ".." = ParentDir);
    path_comp_test!(
        abs_simple,
        "/abc/def/ghi" = Root,
        Name("abc"),
        Name("def"),
        Name("ghi")
    );
    path_comp_test!(
        abs_simple_dir,
        "/abc/def/ghi/" = Root,
        Name("abc"),
        Name("def"),
        Name("ghi")
    );
    path_comp_test!(
        abs_with_dots,
        "/abc/../ghi/." = Root,
        Name("abc"),
        ParentDir,
        Name("ghi"),
        CurrentDir
    );
    path_comp_test!(
        rel_simple,
        "abc/def/ghi" = Name("abc"),
        Name("def"),
        Name("ghi")
    );
}
