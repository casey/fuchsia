// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Utilities for safely operating on memory shared between untrusting
//! processes.
//!
//! `shared-buffer` provides support for safely operating on memory buffers
//! which are shared with another process which is untrusted. The Rust memory
//! model assumes that only code running in the current process - and thus
//! either trusted or generated by Rust itself - operates on a given region of
//! memory. As a result, simply treating a region of memory to which another,
//! untrusted process has read or write access as equivalent to normal process
//! memory is unsafe. This crate provides the `SharedBuffer` type, which has
//! methods that allow safe access to such memory.
//!
//! Examples of issues that could arise if shared memory were treated as normal
//! memory include:
//! - Unintentionally leaking sensitive values to another process
//! - Allowing other processes to cause an invalid sequence of memory to be
//!   interpreted as a given type

// NOTES(joshlf) on implementation: We need to worry about the following issues:
// - If another process has write access to a given region of memory, then
//   arbitrary writes may happen at any time. Thus, it is never safe to access
//   this memory through any Rust type other than a raw pointer, or else the
//   compiler might allow operations or make optimizations based on the
//   assumption that the memory is either owned (in the case of a mutable
//   reference) or immutable (in the case of an immutable reference). In either
//   of these cases, any such allowance or optimization would be unsound. For
//   example, the compiler might decide that, after having written a T to a
//   particular memory location, it is safe to read that memory location and
//   treat it as a T. This would cause undefined behavior if the other process
//   modified that memory location in the meantime. Perhaps more fundamentally,
//   both mutable and immutable references guarantee that nobody else is
//   modifying this memory other than me (and not even me, in the case of an
//   immutable reference). On this basis alone, it is clear that neither
//   reference is compatible with foreign write access to the referent.
// - If another process has read access to a given region of memory, then it
//   cannot affect the correctness of a Rust program. However, it can do things
//   that do not technically violate correctness, but are still undesirable. The
//   canonical example is reading memory which contains sensitive information.
//   Even if the programmer were to construct a mutable reference to such memory
//   and write a value to it which the programmer intended to be shared with the
//   other process, the compiler might use the fact that it had exclusive access
//   to the memory (so says the mutable reference...) to store any arbitrary
//   value in the memory temporarily. So long as it's not observable from the
//   Rust program, it preserves the semantics of the program. Of course, it *is*
//   observable from the other process, and there are no guarantees on what the
//   compiler might decide to store there, including any value currently in your
//   memory space, including particularly sensitive values. As a result, while
//   read-only access doesn't violate the correctness of a Rust program, it's
//   still worth handling carefully.
//
// In order to address both of these issues, our approach is simple: never treat
// the memory as anything other than a raw pointer. Do not construct any
// references, mutable or immutable, even temporarily, and even if they are
// never used. This basically boils down to only accessing the memory using the
// various functions from core::ptr which operate directly on raw pointers.

// NOTE(joshlf):
// - Since you must assume that the other process might be writing to the
//   memory, there's no point in having exclusive access. Thus, we implement
//   Clone and provide slicing methods. There's no point not to.
// - Since all access to these buffers must go through the methods of
//   SharedBuffer, correct code may not construct a reference to this memory.
//   Thus, the references to dst and src passed to read, read_at, write, and
//   write_at cannot overlap with the buffer itself, and so it's safe to use
//   ptr::copy_nonoverlapping.
// - Note on volatility and observability: The memory in a SharedBuffer is
//   either allocated by this process and then sent to another process, or
//   allocated by another process and sent to this process. However, on Fuchsia,
//   what's actually shared is a VMO, which is then mapped into the address
//   space. While LLVM is almost certainly guaranteed to treat this call as
//   opaque, and thus to be unable to prove to itself that the returned memory
//   is not shared, it is worth hedging against that reasoning being wrong. If
//   LLVM were, for some reason, to decide that mapping a VMO resulted in
//   uniquely owned memory, it would be able to reason that writes to that
//   memory could never be observed by other threads, and so if the writes were
//   not observed by the _current_ thread, they could be elided altogether since
//   they could have no effect. In order to hedge against this possibility, and
//   to ensure that LLVM definitely cannot take this line of reasoning, we
//   volatile write the pointer when we first construct the SharedBuffer. LLVM
//   must conclude that it doesn't know who else is using the memory once a
//   pointer to it has been written in a volatile manner, and so must assume
//   that all future writes must be observable. This single volatile write which
//   happens at most once per message (although more likely once when the
//   connection is first established) has minimal performance overhead.

// TODO(joshlf):
// - Create a variant for read-only memory
// - Create a variant for write-only memory?

#![no_std]

use core::marker::PhantomData;
use core::ptr;
use core::sync::atomic::{fence, Ordering};

/// A shared region of memory.
///
/// A `SharedBuffer` is a view into a region of memory to which another process
/// has access. It provides methods to access this memory in a way that
/// preserves memory safety.
///
/// Since the buffer is shared by an untrusted process, it is never valid to
/// assume that a given region of the buffer will not change in between method
/// calls. Even if no thread in this process wrote anything to the buffer, the
/// other process might have.
#[derive(Clone)]
pub struct SharedBuffer<'a> {
    // invariant: '(buf as usize) + len' doesn't overflow usize
    buf: *mut u8,
    len: usize,
    _marker: PhantomData<&'a ()>,
}

impl<'a> SharedBuffer<'a> {
    /// Create a new `SharedBuffer` from a raw buffer.
    ///
    /// `new` creates a new `SharedBuffer` from the provided buffer and lenth.
    /// `SharedBuffer`s have a lifetime parameter which may be used to ensure
    /// that a `SharedBuffer` does not live beyond the point at which memory is
    /// either unmapped or semantically returned to another process.
    ///
    /// # Safety
    ///
    /// Memory in a shared buffer must never be accessed except through the
    /// methods of `SharedBuffer`. It must not be treated as normal memory, and
    /// pointers to it must not be passed to unsafe code which is designed to
    /// operate on normal memory. It must be guaranteed that, for the lifetime
    /// of the `SharedBuffer`, the memory region is mapped, readable, and
    /// writable.
    ///
    /// If any of these guarantees are violated, it may cause undefined
    /// behavior.
    pub unsafe fn new(buf: *mut u8, len: usize) -> SharedBuffer<'a> {
        // Write the pointer and the length using a volatile write so that LLVM
        // must assume that the memory has escaped, and that all future writes
        // to it are observable. See the NOTE above for more details.
        let mut scratch = (ptr::null_mut(), 0);
        ptr::write_volatile(&mut scratch, (buf, len));
        SharedBuffer {
            buf,
            len,
            _marker: PhantomData,
        }
    }

    /// Read bytes from the buffer.
    ///
    /// Read up to `dst.len()` bytes from the buffer, returning how many bytes
    /// were read. The only thing that can cause fewer bytes to be read than
    /// requested is if `dst` is larger than the buffer itself.
    #[inline]
    pub fn read(&self, dst: &mut [u8]) -> usize {
        self.read_at(0, dst)
    }

    /// Read bytes from the buffer at an offset.
    ///
    /// Read up to `dst.len()` bytes starting at `offset` into the buffer,
    /// returning how many bytes were read. The only thing that can cause fewer
    /// bytes to be read than requested is if there are fewer than `dst.len()`
    /// bytes available starting at `offset` within the buffer.
    ///
    /// # Panics
    ///
    /// `read_at` panics if `offset` is greater than the length of the buffer.
    #[inline]
    pub fn read_at(&self, offset: usize, dst: &mut [u8]) -> usize {
        if let Some(to_copy) = overlap(offset, dst.len(), self.len) {
            // Since overlap returned Some, we're guaranteed that 'offset +
            // to_copy <= self.len'. That in turn means that, so long as the
            // invariant holds that '(self.buf as usize) + self.len' doesn't
            // overflow usize, then this call to offset_from won't overflow, and
            // neither will the call to copy_nonoverlapping.
            let base = offset_from(self.buf, offset);
            unsafe { ptr::copy_nonoverlapping(base, dst.as_mut_ptr(), to_copy) };
            to_copy
        } else {
            panic!(
                "byte offset {} out of range for SharedBuffer of length {}",
                offset, self.len
            );
        }
    }

    /// Write bytes to the buffer.
    ///
    /// Write up to `src.len()` bytes into the buffer, returning how many bytes
    /// were written. The only thing that can cause fewer bytes to be written
    /// than requested is if `src` is larger than the buffer itself.
    ///
    /// A call to `write` is only guaranteed to happen before an operation in
    /// another thread or process if the mechanism used to signal the other
    /// process has well-defined memory ordering semantics. Otherwise, the
    /// `release_writes` method must be called after `write` and before
    /// signalling the other process in order to provide such ordering
    /// guarantees. In practice, this means that `release_writes` should be the
    /// last write operation that happens before signalling another process that
    /// the memory may be read. See the `release_writes` documentation for more
    /// details.
    #[inline]
    pub fn write(&self, src: &[u8]) -> usize {
        self.write_at(0, src)
    }

    /// Write bytes to the buffer at an offset.
    ///
    /// Write up to `src.len()` bytes starting at `offset` into the buffer,
    /// returning how many bytes were written. The only thing that can cause
    /// fewer bytes to be written than requested is if there are fewer than
    /// `src.len()` bytes available starting at `offset` within the buffer.
    ///
    /// A call to `write_at` is only guaranteed to happen before an operation in
    /// another thread or process if the mechanism used to signal the other
    /// process has well-defined memory ordering semantics. Otherwise, the
    /// `release_writes` method must be called after `write_at` and before
    /// signalling the other process in order to provide such ordering
    /// guarantees. In practice, this means that `release_writes` should be the
    /// last write operation that happens before signalling another process that
    /// the memory may be read. See the `release_writes` documentation for more
    /// details.
    ///
    /// # Panics
    ///
    /// `write_at` panics if `offset` is greater than the length of the buffer.
    #[inline]
    pub fn write_at(&self, offset: usize, src: &[u8]) -> usize {
        if let Some(to_copy) = overlap(offset, src.len(), self.len) {
            // Since overlap returned Some, we're guaranteed that 'offset +
            // to_copy <= self.len'. That in turn means that, so long as the
            // invariant holds that '(self.buf as usize) + self.len' doesn't
            // overflow usize, then this call to offset_from won't overflow, and
            // neither will the call to copy_nonoverlapping.
            let base = offset_from(self.buf, offset);
            unsafe { ptr::copy_nonoverlapping(src.as_ptr(), base, to_copy) };
            to_copy
        } else {
            panic!(
                "byte offset {} out of range for SharedBuffer of length {}",
                offset, self.len
            );
        }
    }

    /// Atomically release all writes performed so far.
    ///
    /// On some systems (such as Fuchsia, currently), the communication
    /// mechanism used for signalling the other process that memory is readable
    /// does not have well-defined synchronization semantics. On those systems,
    /// this method MUST be called before such signalling, or else writes
    /// performed before that signal are not guaranteed to be observed by the
    /// other process.
    ///
    /// # Note on Fuchsia
    ///
    /// Zircon, the Fuchsia kernel, will likely eventually have well-defined
    /// semantics around the synchronization behavior of various syscalls. Once
    /// that happens, calling this method in Fuchsia programs may become
    /// optional. This work is tracked in [ZX-2239].
    ///
    /// [ZX-2239]: #
    // // TODO(joshlf): Replace with link once issues are public.
    pub fn release_writes(&self) {
        fence(Ordering::Release);
    }

    /// Create a slice of the original `SharedBuffer`.
    ///
    /// Just like the slicing operation on array and slice references, `slice`
    /// constructs a new `SharedBuffer` which points to the same memory as the
    /// original, but starting and index `from` (inclusive) and ending at index
    /// `to` (exclusive).
    ///
    /// # Panics
    ///
    /// `slice` panics if `to` is larger than the size of the `SharedBuffer` or
    /// if `from > to`.
    #[inline]
    pub fn slice(&self, from: usize, to: usize) -> SharedBuffer<'a> {
        if to > self.len {
            panic!(
                "byte index {} out of range for SharedBuffer of length {}",
                to, self.len
            );
        }
        if from > to {
            panic!("slice starts at byte {} but ends at byte {}", from, to);
        }

        let len = to - from;
        let buf = offset_from(self.buf, from);
        SharedBuffer {
            buf,
            len,
            _marker: PhantomData,
        }
    }

    /// The number of bytes in this `SharedBuffer`.
    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }
}

// Verifies that 'offset' is in range of range_len (that 'offset <= range_len'),
// and returns the amount of overlap between a copy of length 'copy_len'
// starting at 'offset' and a buffer of length 'range_len'. The number it
// returns is guaranteed to be less than or equal to 'range_len'.
//
// overlap is guaranteed to be correct for any three usize values.
fn overlap(offset: usize, copy_len: usize, range_len: usize) -> Option<usize> {
    if offset > range_len {
        None
    } else if offset
        .checked_add(copy_len)
        .map(|sum| sum <= range_len)
        .unwrap_or(false)
    {
        // if 'offset + copy_len' overflows usize, then 'offset + copy_len >
        // range_len', so we unwrap_or(false)
        Some(copy_len)
    } else {
        Some(range_len - offset)
    }
}

// Like the offset method on primitive pointers, but for unsigned offsets. Both
// the 'offset' and 'add' methods on primitive pointers have the limitation that
// the offset cannot overflow an isize or else it will cause UB. offset_from
// function has no such restriction.
//
// The caller must guarantee that '(ptr as usize) + offset' doesn't overflow
// usize.
fn offset_from(ptr: *mut u8, offset: usize) -> *mut u8 {
    // just in case our logic is wrong, better to catch it at runtime than
    // invoke UB
    (ptr as usize).checked_add(offset).unwrap() as *mut u8
}

#[cfg(test)]
mod tests {
    use core::mem;
    use core::ptr;

    use {overlap, SharedBuffer};

    // use the referent as the backing memory for a SharedBuffer
    unsafe fn buf_from_ref<'a, T>(x: &'a mut T) -> SharedBuffer<'a> {
        let size = mem::size_of::<T>();
        SharedBuffer::new(x as *mut _ as *mut u8, size)
    }

    #[test]
    fn test_buf() {
        // initialize some memory and turn it into a SharedBuffer
        const ONE: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
        let mut buf_memory = ONE;
        let buf = unsafe { buf_from_ref(&mut buf_memory) };

        // we read the same initial contents back
        let mut bytes = [0u8; 8];
        assert_eq!(buf.read(&mut bytes[..]), 8);
        assert_eq!(bytes, ONE);

        // when we write new contents, we read those back
        const TWO: [u8; 8] = [7, 6, 5, 4, 3, 2, 1, 0];
        assert_eq!(buf.write(&TWO[..]), 8);
        assert_eq!(buf.read(&mut bytes[..]), 8);
        assert_eq!(bytes, TWO);

        // even with a bigger buffer, we still only read 8 bytes
        let mut bytes = [0u8; 16];
        assert_eq!(buf.read(&mut bytes[..]), 8);
        // starting at offset 4, there are only 4 bytes left, so we only read 4
        // bytes
        assert_eq!(buf.read_at(4, &mut bytes[..]), 4);
    }

    #[test]
    fn test_slice() {
        // various slices give us the lengths we expect
        let buf = unsafe { SharedBuffer::new(ptr::null_mut(), 10) };
        let tmp = buf.slice(0, 10);
        assert_eq!(tmp.len(), 10);
        let tmp = buf.slice(5, 10);
        assert_eq!(tmp.len(), 5);
        let tmp = buf.slice(0, 0);
        assert_eq!(tmp.len(), 0);
        let tmp = buf.slice(10, 10);
        assert_eq!(tmp.len(), 0);

        // initialize some memory and turn it into a SharedBuffer
        const INIT: [u8; 8] = [0, 1, 2, 3, 4, 5, 6, 7];
        let mut buf_memory = INIT;
        let buf = unsafe { buf_from_ref(&mut buf_memory) };

        // we read the same initial contents back
        let mut bytes = [0u8; 8];
        assert_eq!(buf.read_at(0, &mut bytes[..]), 8);
        assert_eq!(bytes, INIT);

        // create a slice to the second half of the SharedBuffer
        let buf2 = buf.slice(4, 8);

        // now we read back only the second half of the original SharedBuffer
        bytes = [0; 8];
        assert_eq!(buf2.read(&mut bytes[..]), 4);
        assert_eq!(bytes, [4, 5, 6, 7, 0, 0, 0, 0]);
    }

    #[test]
    fn test_overlap() {
        // overlap(offset, copy_len, range_len)

        // first branch: offset > range_len
        assert_eq!(overlap(10, 4, 8), None);

        // middle branch: offset + copy_len <= range_len
        assert_eq!(overlap(0, 4, 8), Some(4));
        assert_eq!(overlap(4, 4, 8), Some(4));

        // middle branch: 'offset + copy_len' overflows usize
        assert_eq!(overlap(4, ::core::usize::MAX, 8), Some(4));

        // last branch: else
        assert_eq!(overlap(6, 4, 8), Some(2));
        assert_eq!(overlap(8, 4, 8), Some(0));
    }

    #[test]
    #[should_panic]
    fn test_panic_read_at() {
        let buf = unsafe { SharedBuffer::new(ptr::null_mut(), 10) };
        // "byte offset 11 out of range for SharedBuffer of length 10"
        buf.read_at(11, &mut [][..]);
    }

    #[test]
    #[should_panic]
    fn test_panic_write_at() {
        let buf = unsafe { SharedBuffer::new(ptr::null_mut(), 10) };
        // "byte offset 11 out of range for SharedBuffer of length 10"
        buf.write_at(11, &[][..]);
    }

    #[test]
    #[should_panic]
    fn test_panic_slice_1() {
        let buf = unsafe { SharedBuffer::new(ptr::null_mut(), 10) };
        // "byte index 11 out of range for SharedBuffer of length 10"
        buf.slice(0, 11);
    }

    #[test]
    #[should_panic]
    fn test_panic_slice_2() {
        let buf = unsafe { SharedBuffer::new(ptr::null_mut(), 10) };
        // "slice starts at byte 6 but ends at byte 5"
        buf.slice(6, 5);
    }
}
