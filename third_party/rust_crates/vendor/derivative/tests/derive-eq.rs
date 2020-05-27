#[cfg(feature = "use_core")]
extern crate core;

#[macro_use]
extern crate derivative;

#[derive(Derivative, PartialEq)]
#[derivative(Eq)]
struct Foo {
    foo: u8
}

#[derive(Derivative)]
#[derivative(Eq)]
struct WithPtr<T: ?Sized> {
    #[derivative(Eq(bound=""))]
    foo: *const T
}

impl<T: ?Sized> PartialEq for WithPtr<T> {
    fn eq(&self, other: &Self) -> bool {
        self.foo == other.foo
    }
}

trait SomeTrait {}
struct SomeType {
    #[allow(dead_code)]
    foo: u8
}
impl SomeTrait for SomeType {}

fn assert_eq<T: Eq>(_: T) {}

#[test]
fn main() {
    assert!(Foo { foo: 7 } == Foo { foo: 7 });
    assert!(Foo { foo: 7 } != Foo { foo: 42 });

    assert_eq(Foo { foo: 7 });

    let ptr1: *const dyn SomeTrait = &SomeType { foo: 0 };
    let ptr2: *const dyn SomeTrait = &SomeType { foo: 1 };
    assert!(WithPtr { foo: ptr1 } == WithPtr { foo: ptr1 });
    assert!(WithPtr { foo: ptr1 } != WithPtr { foo: ptr2 });

    assert_eq(WithPtr { foo: ptr1 });
}
