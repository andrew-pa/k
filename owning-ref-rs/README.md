This source code originally came from https://github.com/Kimundi/owning-ref-rs. I ported it to the no-std environment and then addressed some of the many soundness issues with the crate by the highly effective method of commenting out everything except what is necessary for `chashmap` as well as including the ideas in some of the oustanding PRs on the original repository. Long-term it would probably be best to create a new fork and merge many of the outstanding PRs, but for now this hack works. An even nicer solution might be rewriting `chashmap` so that this crate is no longer necessary.

---

[![Build Status](https://travis-ci.org/Kimundi/owning-ref-rs.svg)](https://travis-ci.org/Kimundi/owning-ref-rs)
[![Crate](https://img.shields.io/crates/v/owning_ref.svg)](https://crates.io/crates/owning_ref)
[![Docs](https://docs.rs/owning_ref/badge.svg)](https://docs.rs/owning_ref)

owning-ref-rs
==============

A library for creating references that carry their owner with them.

This can sometimes be useful because Rust borrowing rules normally prevent
moving a type that has been borrowed from. For example, this kind of code gets rejected:

```rust
fn return_owned_and_referenced<'a>() -> (Vec<u8>, &'a [u8]) {
    let v = vec![1, 2, 3, 4];
    let s = &v[1..3];
    (v, s)
}
```

This library enables this safe usage by keeping the owner and the reference
bundled together in a wrapper type that ensure that lifetime constraint:

```rust
fn return_owned_and_referenced() -> OwningRef<Vec<u8>, [u8]> {
    let v = vec![1, 2, 3, 4];
    let or = OwningRef::new(v);
    let or = or.map(|v| &v[1..3]);
    or
}
```

## Getting Started

To get started, add the following to `Cargo.toml`.

```toml
owning_ref = "0.4.1"
```

...and see the [docs](http://kimundi.github.io/owning-ref-rs/owning_ref/index.html) for how to use it.


## Example

```rust
extern crate owning_ref;
use owning_ref::BoxRef;

fn main() {
    // Create an array owned by a Box.
    let arr = Box::new([1, 2, 3, 4]) as Box<[i32]>;

    // Transfer into a BoxRef.
    let arr: BoxRef<[i32]> = BoxRef::new(arr);
    assert_eq!(&*arr, &[1, 2, 3, 4]);

    // We can slice the array without losing ownership or changing type.
    let arr: BoxRef<[i32]> = arr.map(|arr| &arr[1..3]);
    assert_eq!(&*arr, &[2, 3]);

    // Also works for Arc, Rc, String and Vec!
}
```
