extern crate proc_macro;
use proc_macro::TokenStream;

use syn::{parse_macro_input, DeriveInput};
use quote::{format_ident, quote};

#[proc_macro_derive(Prefix)]
pub fn derive(input: TokenStream) -> TokenStream {
    let macro_input = parse_macro_input!(input as DeriveInput);
    let name = macro_input.ident;

    let data = if let syn::Data::Enum(data) = macro_input.data {
        data
    } else {
        unimplemented!();
    };

    let variants = data.variants.iter().enumerate().map(|(i, variant)| {
        let name = &variant.ident;
        let field_definitions = variant.fields.iter().enumerate().map(|(i, _)| {
            let ident = format_ident!("_{i}");
            quote! { #ident }
        });

        let discriminant = i;

        if field_definitions.len() > 0 {
            let field_statements = variant.fields.iter().enumerate().map(|(i, _)| {
                let ident = format_ident!("_{i}");
                quote! {
                    prefix.push(b'_');
                    prefix.extend(#ident.to_le_bytes());
                }
            });

            quote! {
                Self::#name( #(#field_definitions),* ) => {
                    let mut prefix = #discriminant.to_le_bytes().to_vec();

                    #(#field_statements)*

                    prefix
                },
            }
        } else {
            quote! {
                Self::#name => {
                    #discriminant.to_le_bytes().to_vec()
                },
            }
        }
    });

    let expanded = quote! (
        impl Prefix for #name {
            fn to_bytes(&self) -> Vec<u8> {
                match self {
                    #(#variants)*
                }
            }
        }
    );

    expanded.into()
}