extern crate proc_macro;

use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, ItemFn};
#[cfg(feature = "tracing")]
use syn::{AttributeArgs, Meta, NestedMeta};

/// proc macro to instrument functions with tracing.
///
/// # Arguments
///
/// - `args`: Attributes to pass to the `#[tracing::instrument]` macro.
/// - `input`: The function to be instrumented.
///
/// # Example
///
/// ```
/// #[shim_instrument(level = "Info")]
/// fn my_function() {
///     // function body
/// }
/// ```
#[proc_macro_attribute]
pub fn shim_instrument(args: TokenStream, input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as ItemFn);

    #[cfg(feature = "tracing")]
    let output = {
        let args = parse_macro_input!(args as AttributeArgs);
        let span_parent = quote! { parent = tracing::Span::current(), };
        let attrs = args.iter().map(|arg| match arg {
            NestedMeta::Meta(Meta::NameValue(nv)) => quote! { #nv },
            NestedMeta::Meta(Meta::Path(path)) => quote! { #path },
            _ => quote! {},
        });
        let attrs = quote! { #span_parent #(, #attrs)* };
        let fn_block = &input.block;
        let fn_attrs = &input.attrs;
        let fn_vis = &input.vis;
        let fn_sig = &input.sig;

        quote! {
            #[tracing::instrument(#attrs)]
            #(#fn_attrs)*
            #fn_vis #fn_sig #fn_block
        }
    };

    #[cfg(not(feature = "tracing"))]
    let output = {
        quote! {
            #input
        }
    };

    TokenStream::from(output)
}
