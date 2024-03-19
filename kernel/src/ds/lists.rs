//! Concurrent, lock free linked list.
use core::{
    ptr::{addr_of, null_mut},
    sync::atomic::{AtomicPtr, AtomicUsize, Ordering},
};

use alloc::boxed::Box;

struct Node<T> {
    data: T,
    next: AtomicPtr<Node<T>>,
}

impl<T> Node<T> {
    fn new(data: T) -> Self {
        Self {
            data,
            next: AtomicPtr::new(null_mut()),
        }
    }
}

/// A singly linked list that supports safe concurrent operations without locks.
pub struct ConcurrentLinkedList<T> {
    head: AtomicPtr<Node<T>>,
    readers: AtomicUsize,
    pending_deletion: AtomicPtr<Node<Box<Node<T>>>>,
}

pub struct Iter<'p, T> {
    current: *mut Node<T>,
    parent: &'p ConcurrentLinkedList<T>,
}

impl<T> Default for ConcurrentLinkedList<T> {
    fn default() -> Self {
        Self {
            head: AtomicPtr::new(null_mut()),
            readers: AtomicUsize::new(0),
            pending_deletion: AtomicPtr::new(null_mut()),
        }
    }
}

impl<T> ConcurrentLinkedList<T> {
    /// Adds an element to the beginning of the list.
    pub fn push(&self, data: T) {
        let new_node = Box::into_raw(Box::new(Node::new(data)));
        let mut current_head = self.head.load(Ordering::Acquire);
        loop {
            unsafe {
                (*new_node).next.store(current_head, Ordering::Relaxed);
            }
            match self.head.compare_exchange(
                current_head,
                new_node,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(h) => current_head = h,
            }
        }
    }

    /// Adds an owned node to the beginning of the pending deletion pool if there are concurrent
    /// readers to the list. Otherwise the node is immediately deleted.
    fn push_pending_deletion(&self, data: Box<Node<T>>) {
        if self.readers.load(Ordering::Acquire) == 0 {
            drop(data);
            return;
        }

        let new_node = Box::into_raw(Box::new(Node::new(data)));
        let mut current_head = self.pending_deletion.load(Ordering::Acquire);
        loop {
            unsafe {
                (*new_node).next.store(current_head, Ordering::Relaxed);
            }
            match self.pending_deletion.compare_exchange(
                current_head,
                new_node,
                Ordering::Release,
                Ordering::Relaxed,
            ) {
                Ok(_) => return,
                Err(h) => current_head = h,
            }
        }
    }

    /// Removes the first element in the list that matches the predicate and returns it.
    pub fn remove(&self, predicate: impl Fn(&T) -> bool) -> bool {
        // check the head and remove it if it matches the predicate
        let mut head_ptr = self.head.load(Ordering::Acquire);
        loop {
            match unsafe { head_ptr.as_ref() } {
                Some(head) => {
                    if predicate(&head.data) {
                        let next = head.next.load(Ordering::Acquire);
                        match self.head.compare_exchange(
                            head_ptr,
                            next,
                            Ordering::Release,
                            Ordering::Relaxed,
                        ) {
                            Ok(_) => {
                                let old_head = unsafe { Box::from_raw(head_ptr) };
                                self.push_pending_deletion(old_head);
                                return true;
                            }
                            Err(new_head) => {
                                head_ptr = new_head;
                                continue;
                            }
                        }
                    } else {
                        break;
                    }
                }
                None => return false,
            }
        }

        // process the rest of the list
        // this is a pointer to the next field of the previous node that contains current_ptr (points to current)
        let mut prev_ptr = addr_of!(self.head);
        let mut current_ptr = unsafe { (*prev_ptr).load(Ordering::Acquire) };

        while let Some(current) = unsafe { current_ptr.as_ref() } {
            if predicate(&current.data) {
                let next = current.next.load(Ordering::Acquire);
                let prev = unsafe { prev_ptr.as_ref().expect("prev ptr is non-null") };
                match prev.compare_exchange(current_ptr, next, Ordering::Release, Ordering::Relaxed)
                {
                    Ok(_) => unsafe {
                        let cbox = Box::from_raw(current_ptr);
                        self.push_pending_deletion(cbox);
                        return true;
                    },
                    Err(_) => {
                        // reload the current pointer and retry because someone else
                        // modified the node before we did
                        current_ptr = prev.load(Ordering::Acquire);
                    }
                }
            } else {
                prev_ptr = addr_of!(current.next);
                current_ptr = current.next.load(Ordering::Acquire);
            }
        }

        false
    }

    /// Iterate over the elements of the list.
    /// It is possible that elements that are concurrently removed during the lifetime of the iterator may be returned.
    pub fn iter(&self) -> Iter<T> {
        self.readers.fetch_add(1, Ordering::Acquire);
        Iter {
            current: self.head.load(Ordering::Acquire),
            parent: self,
        }
    }

    fn free_pool(&self) {
        // this is best effort, but eventually they'll all get freed
        let mut head_ptr = self.pending_deletion.load(Ordering::Acquire);
        loop {
            match unsafe { head_ptr.as_ref() } {
                Some(head) => {
                    let next = head.next.load(Ordering::Acquire);
                    match self.pending_deletion.compare_exchange(
                        head_ptr,
                        next,
                        Ordering::Release,
                        Ordering::Relaxed,
                    ) {
                        Ok(_) => {
                            let old_head = unsafe { Box::from_raw(head_ptr) };
                            drop(old_head);
                        }
                        Err(new_head) => {
                            head_ptr = new_head;
                        }
                    }
                }
                None => return,
            }
        }
    }
}

impl<'p, T> Iterator for Iter<'p, T> {
    type Item = &'p T;

    fn next(&mut self) -> Option<Self::Item> {
        match unsafe { self.current.as_ref() } {
            Some(n) => {
                self.current = n.next.load(Ordering::Acquire);
                Some(&n.data)
            }
            None => None,
        }
    }
}

impl<'p, T> Drop for Iter<'p, T> {
    fn drop(&mut self) {
        if self.parent.readers.fetch_sub(1, Ordering::Release) == 1 {
            // we are the last reader, so we can free the pending deletion pool
            self.parent.free_pool();
        }
    }
}

impl<T> Drop for ConcurrentLinkedList<T> {
    fn drop(&mut self) {
        let mut current = self.head.swap(null_mut(), Ordering::AcqRel);
        while !current.is_null() {
            let next = unsafe { (*current).next.load(Ordering::Acquire) };
            unsafe {
                drop(Box::from_raw(current));
            }
            current = next;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn push_single_element() {
        let list = ConcurrentLinkedList::<i32>::default();
        list.push(42);

        assert_eq!(
            unsafe { list.head.load(Ordering::SeqCst).as_ref() }
                .unwrap()
                .data,
            42
        );
    }

    #[test_case]
    fn push_multiple_elements() {
        let list = ConcurrentLinkedList::<i32>::default();
        list.push(1);
        list.push(2);
        list.push(3);

        let mut iter = list.iter();
        assert_eq!(*iter.next().unwrap(), 3);
        assert_eq!(*iter.next().unwrap(), 2);
        assert_eq!(*iter.next().unwrap(), 1);
    }

    #[test_case]
    fn remove_single_element() {
        let list = ConcurrentLinkedList::<i32>::default();
        list.push(42);
        assert!(list.remove(|&x| x == 42));
        assert!(list.iter().next().is_none());
    }

    #[test_case]
    fn remove_elements() {
        let list = ConcurrentLinkedList::default();
        list.push(1);
        list.push(2);
        list.push(3);

        assert!(list.remove(|&x| x == 2));
        let mut iter = list.iter();
        assert_eq!(*iter.next().unwrap(), 3);
        assert_eq!(*iter.next().unwrap(), 1);
        assert!(iter.next().is_none());
    }

    #[test_case]
    fn remove_non_existent_element() {
        let list = ConcurrentLinkedList::<i32>::default();
        list.push(1);
        assert!(!list.remove(|&x| x == 2));
    }

    #[test_case]
    fn iterate_over_empty_list() {
        let list = ConcurrentLinkedList::<i32>::default();
        assert!(list.iter().next().is_none());
    }

    #[test_case]
    fn iterate_over_list() {
        let list = ConcurrentLinkedList::default();
        list.push(1);
        list.push(2);
        list.push(3);

        let items: alloc::vec::Vec<_> = list.iter().cloned().collect();
        assert_eq!(items, alloc::vec![3, 2, 1]);
    }

    #[test_case]
    fn readers_count() {
        let list = ConcurrentLinkedList::default();
        list.push(1);
        let iter1 = list.iter();
        assert_eq!(list.readers.load(Ordering::SeqCst), 1);

        let iter2 = list.iter();
        assert_eq!(list.readers.load(Ordering::SeqCst), 2);

        drop(iter1);
        assert_eq!(list.readers.load(Ordering::SeqCst), 1);

        drop(iter2);
        assert_eq!(list.readers.load(Ordering::SeqCst), 0);
    }

    #[test_case]
    fn pending_deletion() {
        let list = ConcurrentLinkedList::default();
        list.push(1);
        list.push(2);

        {
            let _iter = list.iter(); // Increase readers count
            list.remove(|&x| x == 1);
            // At this point, the node should be in the pending deletion pool
        } // Iterator dropped here, readers count goes to 0, pending deletion should be processed

        assert!(unsafe { list.pending_deletion.load(Ordering::SeqCst).as_ref() }.is_none());
    }

    #[test_case]
    fn drop_list() {
        let list = ConcurrentLinkedList::default();
        list.push(1);
        list.push(2);

        // Explicitly drop the list to test_case if all elements are freed
        drop(list);
        // Can't directly test_case that memory is freed in Rust, but this test ensures no panic or segfault
    }

    #[test_case]
    fn iterator_validity_during_modifications() {
        let list = ConcurrentLinkedList::default();
        list.push(1);
        list.push(2);
        list.push(3);

        let mut iter = list.iter();
        assert_eq!(*iter.next().unwrap(), 3);

        // Modify the list during iteration
        list.push(4);
        list.remove(|&x| x == 1);

        // Continue iteration
        assert_eq!(*iter.next().unwrap(), 2);
        assert!(iter.next().is_none()); // 1 was removed, so should not appear
    }

    #[test_case]
    fn operations_on_empty_list() {
        let list = ConcurrentLinkedList::<i32>::default();

        // Remove from empty list
        assert!(
            !list.remove(|&x| x == 1),
            "Removing from empty list should return false"
        );

        // Iterate over empty list
        let mut iter = list.iter();
        assert!(
            iter.next().is_none(),
            "Iterating over empty list should yield no elements"
        );

        // Push then remove the same element, list should be empty again
        list.push(1);
        assert!(
            list.remove(|&x| x == 1),
            "Should be able to remove the element just added"
        );
        let mut iter = list.iter();
        assert!(
            iter.next().is_none(),
            "List should be empty after adding and then removing an element"
        );
    }

    #[test_case]
    fn repeated_additions_and_removals() {
        let list = ConcurrentLinkedList::default();
        list.push(1);
        list.remove(|&x| x == 1);
        list.push(2);
        list.push(3);
        list.remove(|&x| x == 2);

        // After several additions and removals, ensure the list is consistent
        {
            let mut i = list.iter();
            assert_eq!(i.next(), Some(&3));
            assert_eq!(
                i.next(),
                None,
                "List should contain only the last remaining element"
            );
        }

        // Repeatedly add and remove the same element
        for _ in 0..10 {
            list.push(4);
            assert!(
                list.remove(|&x| x == 4),
                "The element should be present and removable"
            );
        }

        list.remove(|&x| x == 3);

        // Ensure the list is empty again
        assert!(
            list.iter().next().is_none(),
            "List should be empty after repeated additions and removals"
        );
    }
}
