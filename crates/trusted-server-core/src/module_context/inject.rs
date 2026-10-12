//! Calling a module's function with its parameters taken from the request's
//! [`ModuleContext`](super::ModuleContext).
//!
//! A trait method takes a [`ModuleCall`] and the implementation hands its own
//! function to [`ModuleCall::inject`], which is generic where the module type
//! is known.
//!
//! The function takes the module first, then up to eight parameters, each a
//! [`FromModuleContext`] value. [`ModuleCall::inject_with`] passes one argument
//! of the caller's own after the module, for the input that is the module's to
//! act on, such as the request a route answers. The function may be `async`,
//! in which case the future it returns is what comes back.
//!
//! ```
//! use trusted_server_core::evidence::RequestInfo;
//! use trusted_server_core::geo::GeoInfo;
//! use trusted_server_core::module_context::{ModuleCall, Withheld};
//!
//! struct Greeter;
//!
//! impl Greeter {
//!     fn greet(&self, request: &dyn RequestInfo, geo: Option<&GeoInfo>) -> String {
//!         let place = geo.map_or("somewhere", |geo| geo.country.as_str());
//!         format!("{} from {place}", request.user_agent())
//!     }
//!
//!     fn process(&self, call: ModuleCall<'_>) -> Result<String, Withheld> {
//!         call.inject(self, Self::greet)
//!     }
//! }
//! ```

use super::{FromModuleContext, ModuleCall, Withheld};

/// A function core can call with the module and its parameters taken from a
/// [`ModuleCall`].
///
/// Implemented for every function and closure taking `&M` and up to eight
/// [`FromModuleContext`] parameters, so a module never implements it itself.
pub trait Process<'c, M: ?Sized, Args, O> {
    /// Calls the function.
    ///
    /// # Errors
    ///
    /// [`Withheld`] for the first parameter that cannot be passed, in which case
    /// the function is not called.
    fn process(self, module: &'c M, call: &ModuleCall<'c>) -> Result<O, Withheld>;
}

/// A function core can call with the module, one argument of the caller's
/// own, and its parameters taken from a [`ModuleCall`].
///
/// Implemented for every function and closure taking `&M`, the argument, and
/// up to eight [`FromModuleContext`] parameters.
pub trait ProcessWith<'c, M: ?Sized, X, Args, O> {
    /// Calls the function.
    ///
    /// # Errors
    ///
    /// [`Withheld`] for the first parameter that cannot be passed, in which case
    /// the function is not called and the argument is dropped.
    fn process_with(self, module: &'c M, argument: X, call: &ModuleCall<'c>)
    -> Result<O, Withheld>;
}

macro_rules! process {
    ($($parameter:ident),*) => {
        impl<'c, M, F, O, $($parameter,)*> Process<'c, M, ($($parameter,)*), O> for F
        where
            M: ?Sized + 'c,
            F: FnOnce(&'c M, $($parameter,)*) -> O,
            $($parameter: FromModuleContext<'c>,)*
        {
            #[allow(non_snake_case, reason = "the parameters are named by their types")]
            fn process(self, module: &'c M, call: &ModuleCall<'c>) -> Result<O, Withheld> {
                let _ = call;
                $(let $parameter = $parameter::from_module_context(call)?;)*
                Ok(self(module, $($parameter,)*))
            }
        }

        impl<'c, M, X, F, O, $($parameter,)*> ProcessWith<'c, M, X, ($($parameter,)*), O> for F
        where
            M: ?Sized + 'c,
            F: FnOnce(&'c M, X, $($parameter,)*) -> O,
            $($parameter: FromModuleContext<'c>,)*
        {
            #[allow(non_snake_case, reason = "the parameters are named by their types")]
            fn process_with(
                self,
                module: &'c M,
                argument: X,
                call: &ModuleCall<'c>,
            ) -> Result<O, Withheld> {
                let _ = call;
                $(let $parameter = $parameter::from_module_context(call)?;)*
                Ok(self(module, argument, $($parameter,)*))
            }
        }
    };
}

process!();
process!(A1);
process!(A1, A2);
process!(A1, A2, A3);
process!(A1, A2, A3, A4);
process!(A1, A2, A3, A4, A5);
process!(A1, A2, A3, A4, A5, A6);
process!(A1, A2, A3, A4, A5, A6, A7);
process!(A1, A2, A3, A4, A5, A6, A7, A8);

impl<'c> ModuleCall<'c> {
    /// Calls `function` with `module` and each of its other parameters taken
    /// from the context.
    ///
    /// A parameter that is withheld skips the call, which is logged at debug
    /// level with the reason, and an `Option` parameter that is withheld is
    /// `None` instead. See [`module_context`](super) for what is withheld when.
    ///
    /// # Errors
    ///
    /// [`Withheld`] when a parameter cannot be passed, and the function is not
    /// called.
    pub fn inject<M, Args, O, F>(&self, module: &'c M, function: F) -> Result<O, Withheld>
    where
        M: ?Sized,
        F: Process<'c, M, Args, O>,
    {
        function
            .process(module, self)
            .inspect_err(|withheld| self.log_skip(withheld))
    }

    /// Calls `function` with `module`, `argument`, and each of its other
    /// parameters taken from the context, as [`inject`](Self::inject) does.
    ///
    /// # Errors
    ///
    /// [`Withheld`] when a parameter cannot be passed, and the function is not
    /// called.
    pub fn inject_with<M, X, Args, O, F>(
        &self,
        module: &'c M,
        argument: X,
        function: F,
    ) -> Result<O, Withheld>
    where
        M: ?Sized,
        F: ProcessWith<'c, M, X, Args, O>,
    {
        function
            .process_with(module, argument, self)
            .inspect_err(|withheld| self.log_skip(withheld))
    }

    fn log_skip(&self, withheld: &Withheld) {
        log::debug!("Skipping module `{}`: {withheld}", self.module());
    }
}
