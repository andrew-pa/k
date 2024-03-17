use core::{
    ptr::{addr_of, null_mut},
    sync::atomic::{AtomicPtr, Ordering},
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

/// A linked list that supports safe concurrent operations without locks.
pub struct ConcurrentLinkedList<T> {
    head: AtomicPtr<Node<T>>,
}

impl<T> Default for ConcurrentLinkedList<T> {
    fn default() -> Self {
        Self {
            head: AtomicPtr::new(null_mut()),
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

    /// Removes the first element in the list that matches the predicate and returns it.
    pub fn remove(&self, predicate: impl Fn(&T) -> bool) -> Option<T> {
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
                                return Some(old_head.data);
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
                None => return None,
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
                        return Some(cbox.data);
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

        None
    }
}

impl<T: Clone> ConcurrentLinkedList<T> {
    /// Finds the first element that matches the predicate and returns a clone.
    pub fn find(&self, predicate: impl Fn(&T) -> bool) -> Option<T> {
        let mut current = self.head.load(Ordering::Acquire);
        unsafe {
            loop {
                match current.as_ref() {
                    Some(cr) => {
                        if predicate(&cr.data) {
                            return Some(cr.data.clone());
                        }
                        current = cr.next.load(Ordering::Acquire);
                    }
                    None => return None,
                }
            }
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
    fn test_push_single_item() {
        let list = ConcurrentLinkedList::default();
        list.push(10);
        assert_eq!(list.find(|&x| x == 10), Some(10));
    }

    #[test_case]
    fn test_push_multiple_items() {
        let list = ConcurrentLinkedList::default();
        list.push(10);
        list.push(20);
        assert_eq!(list.find(|&x| x == 20), Some(20));
        assert_eq!(list.find(|&x| x == 10), Some(10));
    }

    #[test_case]
    fn test_remove_from_empty_list() {
        let list: ConcurrentLinkedList<i32> = ConcurrentLinkedList::default();
        assert_eq!(list.remove(|&x| x == 10), None);
    }

    #[test_case]
    fn test_remove_item_that_exists() {
        let list = ConcurrentLinkedList::default();
        list.push(10);
        let removed = list.remove(|&x| x == 10);
        assert_eq!(removed, Some(10));
        assert_eq!(list.find(|&x| x == 10), None);
    }

    #[test_case]
    fn test_remove_item_that_does_not_exist() {
        let list = ConcurrentLinkedList::default();
        list.push(10);
        assert_eq!(list.remove(|&x| x == 20), None);
    }

    #[test_case]
    fn test_remove_first_item() {
        let list = ConcurrentLinkedList::default();
        list.push(10);
        list.push(20); // This will be the head now
        assert_eq!(list.remove(|&x| x == 20), Some(20));
        assert_eq!(list.find(|&x| x == 20), None);
        assert_eq!(list.find(|&x| x == 10), Some(10));
    }

    #[test_case]
    fn test_remove_last_item() {
        let list = ConcurrentLinkedList::default();
        list.push(10);
        list.push(20);
        assert_eq!(list.remove(|&x| x == 10), Some(10));
        assert_eq!(list.find(|&x| x == 10), None);
        assert_eq!(list.find(|&x| x == 20), Some(20));
    }

    #[test_case]
    fn test_remove_all_items() {
        let list = ConcurrentLinkedList::default();
        list.push(10);
        list.push(20);
        assert_eq!(list.remove(|&x| x == 20), Some(20));
        assert_eq!(list.remove(|&x| x == 10), Some(10));
        assert_eq!(list.find(|&_| true), None); // Check list is empty
    }

    #[test_case]
    fn test_find_in_empty_list() {
        let list: ConcurrentLinkedList<i32> = ConcurrentLinkedList::default();
        assert_eq!(list.find(|&x| x == 10), None);
    }

    #[test_case]
    fn test_find_existing_item() {
        let list = ConcurrentLinkedList::default();
        list.push(10);
        assert_eq!(list.find(|&x| x == 10), Some(10));
    }

    #[test_case]
    fn test_find_non_existing_item() {
        let list = ConcurrentLinkedList::default();
        list.push(10);
        assert_eq!(list.find(|&x| x == 20), None);
    }

    #[test_case]
    fn test_drop_empty_list() {
        let list: ConcurrentLinkedList<i32> = ConcurrentLinkedList::default();
        drop(list);
        // Success if no panic
    }

    #[test_case]
    fn test_drop_non_empty_list() {
        let list = ConcurrentLinkedList::default();
        list.push(10);
        list.push(20);
        drop(list);
        // Success if no panic; proper cleanup is assumed
    }

    #[test_case]
    fn test_default_constructor() {
        let list: ConcurrentLinkedList<i32> = ConcurrentLinkedList::default();
        assert_eq!(list.find(|&_| true), None); // List should be empty
    }

    #[test_case]
    fn test_list_ordering() {
        let list = ConcurrentLinkedList::default();
        list.push(10);
        list.push(20); // 20 should now be the head
        let first_find = list.find(|&x| x == 20);
        let second_find = list.find(|&x| x == 10);
        assert_eq!(first_find, Some(20));
        assert_eq!(second_find, Some(10));
    }

    #[test_case]
    fn test_list_uniqueness() {
        let list = ConcurrentLinkedList::default();
        list.push(10);
        list.push(10); // Add a duplicate
        assert_eq!(list.remove(|&x| x == 10), Some(10)); // Remove one of the duplicates
        assert_eq!(list.find(|&x| x == 10), Some(10)); // The other should still exist
    }
}
