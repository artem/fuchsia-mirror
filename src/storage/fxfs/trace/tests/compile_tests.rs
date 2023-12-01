// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fxfs_trace::{cstr, FxfsTraceFutureExt};

#[fuchsia::test]
fn test_fn_attr_sync() {
    #[fxfs_trace::trace]
    fn test_fn() -> u64 {
        5
    }
    assert_eq!(test_fn(), 5);
}

#[fuchsia::test]
async fn test_fn_attr_async() {
    #[fxfs_trace::trace]
    async fn test_fn() -> u64 {
        5
    }
    assert_eq!(test_fn().await, 5);
}

#[fuchsia::test]
async fn test_fn_attr_async_with_dyn_return_type() {
    trait TestTrait {}
    struct S1;
    impl TestTrait for S1 {}
    struct S2;
    impl TestTrait for S2 {}

    #[fxfs_trace::trace]
    async fn test_fn(b: bool) -> Box<dyn TestTrait> {
        if b {
            return Box::new(S1);
        } else {
            return Box::new(S2);
        }
    }
    test_fn(true).await;
}

#[fuchsia::test]
async fn test_fn_attr_async_with_impl_return_type() {
    trait TestTrait {}
    struct S1;
    impl TestTrait for S1 {}

    #[fxfs_trace::trace]
    async fn test_fn() -> impl TestTrait {
        S1
    }
    test_fn().await;
}

#[fuchsia::test]
async fn test_fn_attr_async_with_no_return_type() {
    #[fxfs_trace::trace]
    async fn test_fn() {}
    test_fn().await;
}

#[fuchsia::test]
fn test_fn_attr_with_name() {
    #[fxfs_trace::trace(name = "trace-name")]
    fn test_fn() -> u64 {
        5
    }
    assert_eq!(test_fn(), 5);
}

#[fuchsia::test]
fn test_impl_attr_with_trace_method_sync() {
    struct Foo;
    #[fxfs_trace::trace]
    impl Foo {
        #[trace]
        fn test_fn(&self) -> u64 {
            5
        }
    }
    assert_eq!(Foo.test_fn(), 5);
}

#[fuchsia::test]
async fn test_impl_attr_with_trace_method_async() {
    struct Foo;
    #[fxfs_trace::trace]
    impl Foo {
        #[trace]
        async fn test_fn(&self) -> u64 {
            5
        }
    }
    assert_eq!(Foo.test_fn().await, 5);
}

#[fuchsia::test]
fn test_impl_attr_with_prefix() {
    struct Foo;
    #[fxfs_trace::trace(prefix = "Bar")]
    impl Foo {
        #[trace]
        fn test_fn(&self) -> u64 {
            5
        }
    }
    assert_eq!(Foo.test_fn(), 5);
}

#[fuchsia::test]
fn test_impl_attr_with_name() {
    struct Foo;
    #[fxfs_trace::trace]
    impl Foo {
        #[trace(name = "name-override")]
        fn test_fn(&self) -> u64 {
            5
        }
    }
    assert_eq!(Foo.test_fn(), 5);
}

#[fuchsia::test]
fn test_impl_attr_with_prefix_and_name() {
    struct Foo;
    #[fxfs_trace::trace(prefix = "Bar")]
    impl Foo {
        #[trace(name = "name-override")]
        fn test_fn(&self) -> u64 {
            5
        }
    }
    assert_eq!(Foo.test_fn(), 5);
}

#[fuchsia::test]
fn test_impl_attr_with_trace_all_methods() {
    struct Foo;
    #[fxfs_trace::trace(trace_all_methods)]
    impl Foo {
        fn test_fn(&self) -> u64 {
            5
        }
    }
    assert_eq!(Foo.test_fn(), 5);
}

#[fuchsia::test]
fn test_impl_attr_with_trace_all_methods_and_prefix() {
    struct Foo;
    #[fxfs_trace::trace(trace_all_methods, prefix = "Bar")]
    impl Foo {
        fn test_fn(&self) -> u64 {
            5
        }
    }
    assert_eq!(Foo.test_fn(), 5);
}

#[fuchsia::test]
fn test_impl_attr_with_trace_all_methods_and_name() {
    struct Foo;
    #[fxfs_trace::trace(trace_all_methods)]
    impl Foo {
        #[trace(name = "name-override")]
        fn test_fn(&self) -> u64 {
            5
        }
    }
    assert_eq!(Foo.test_fn(), 5);
}

#[fuchsia::test]
fn test_duration() {
    let tace_only_var = 6;
    fxfs_trace::duration!("some-duration");
    fxfs_trace::duration!("some-duration", "arg" => 5);
    fxfs_trace::duration!("some-duration", "arg" => 5, "arg2" => tace_only_var);
}

#[fuchsia::test]
fn test_instant() {
    let tace_only_var = 6;
    fxfs_trace::instant!("some-instant");
    fxfs_trace::instant!("some-instant", "arg" => 5);
    fxfs_trace::instant!("some-instant", "arg" => 5, "arg2" => tace_only_var);
}

#[fuchsia::test]
fn test_flow_begin() {
    let tace_only_var = 6;
    let flow_id = 5u64;
    fxfs_trace::flow_begin!("some-flow", flow_id);
    fxfs_trace::flow_begin!("some-flow", flow_id, "arg" => 5);
    fxfs_trace::flow_begin!("some-flow", flow_id, "arg" => 5, "arg2" => tace_only_var);
}

#[fuchsia::test]
fn test_flow_step() {
    let tace_only_var = 6;
    let flow_id = 5u64;
    fxfs_trace::flow_step!("some-flow", flow_id);
    fxfs_trace::flow_step!("some-flow", flow_id, "arg" => 5);
    fxfs_trace::flow_step!("some-flow", flow_id, "arg" => 5, "arg2" => tace_only_var);
}

#[fuchsia::test]
fn test_flow_end() {
    let tace_only_var = 6;
    let flow_id = 5u64;
    fxfs_trace::flow_end!("some-flow", flow_id);
    fxfs_trace::flow_end!("some-flow", flow_id, "arg" => 5);
    fxfs_trace::flow_end!("some-flow", flow_id, "arg" => 5, "arg2" => tace_only_var);
}

#[fuchsia::test]
async fn test_trace_future() {
    let value = async move { 5 }.trace(cstr!("test-future")).await;
    assert_eq!(value, 5);
}
