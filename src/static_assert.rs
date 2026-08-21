// Copyright 2026 Mist Tecnologia LTDA. All rights reserved.
// SPDX-License-Identifier: Apache-2.0

//! Compile-time assertions.
//!
//! Layout and ABI assumptions belong in the compiler, not in a test that may
//! not run. Two places in this tree have them: the `extern "C"` declarations in
//! [`crate::platform`], where Rust's view of a type has to agree with C's, and
//! the pipeline's per-line structs, where a silently grown field is a
//! throughput regression that the benchmark would report only as a mystery.

/// Compile-time assertion.
/// Fails to compile if the condition is false.
#[macro_export]
macro_rules! static_assert {
    ($x:expr $(,)?) => {
        const _: [(); 0 - !{
            const ASSERT: bool = $x;
            ASSERT
        } as usize] = [];
    };
}

/// Compile-time assertion that a type's size is <= `max_size` and alignment == `expected_align`.
#[macro_export]
macro_rules! static_assert_size_and_align {
    ($ty:ty, $max_size:expr, $expected_align:expr $(,)?) => {
        $crate::static_assert!(core::mem::size_of::<$ty>() <= $max_size as usize);
        $crate::static_assert!(core::mem::align_of::<$ty>() == $expected_align as usize);
    };
}

#[cfg(test)]
mod tests {
    // The macros are checked by the compiler, so the only thing a test can add
    // is proof that they accept what they should: a true condition compiles
    // away to nothing, and a type that meets its bound passes.
    static_assert!(core::mem::size_of::<u64>() == 8);
    static_assert_size_and_align!(u64, 8, 8);
    static_assert_size_and_align!(u32, 8, 4);

    #[test]
    fn assertions_above_compiled() {
        // Reaching this point means every static_assert! in this module held at
        // compile time. A false one would have failed the build.
    }
}
