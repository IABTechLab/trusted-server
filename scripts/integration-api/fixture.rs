//! Public API gate fixture.
mod private_owner {
    #[derive(Clone)]
    pub struct Thing<T> {
        pub value: T,
        hidden: bool,
    }
    impl<T> Thing<T> {
        pub const LIMIT: usize = 4;
        pub fn get(&self) -> &T {
            &self.value
        }
        pub(crate) fn internal(&self) {}
    }
    pub trait Service {
        type Output;
        const MODE: usize = 1;
        fn run(&self, input: u8) -> Self::Output;
        fn defaulted(&self) {}
    }
    impl<T: Clone> Service for Thing<T> {
        type Output = T;
        fn run(&self, _: u8) -> T {
            self.value.clone()
        }
    }
}
pub use private_owner::{Service, Thing as PublicThing};
pub mod visible {
    pub struct Other;
    pub type Name = u64;
}
pub use visible::*;
pub struct Tuple(pub u8, u16);
pub enum Event {
    Unit,
    Data { value: u8 },
    Tuple(u8, String),
}
pub fn make() -> PublicThing<u8> {
    unimplemented!()
}
pub(crate) fn widening_canary() {}
#[doc(hidden)]
pub fn hidden_export() {}
#[cfg(target_arch = "wasm32")]
pub fn target_only() {}
#[macro_export]
macro_rules! public_macro {
    () => {
        1
    };
}
