//! Derive macros for rustclamp. Use them through `rustclamp`, which
//! re-exports each next to its trait: `rustclamp::db::Model`.

use proc_macro::TokenStream;
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitStr, parse_macro_input};

/// Implements `rustclamp::db::Model` for a struct with named fields: each
/// field is read from the column of the same name.
///
/// ```ignore
/// #[derive(Model)]
/// #[model(table = "posts")]
/// pub struct Post {
///     pub id: i64,
///     pub title: String,
///     pub published_at: Option<String>,
/// }
/// ```
///
/// The table is always named: a guessed plural would get `categories` wrong.
#[proc_macro_derive(Model, attributes(model))]
pub fn derive_model(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    model(&input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn model(input: &DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let mut table = None;
    for attribute in input.attrs.iter().filter(|a| a.path().is_ident("model")) {
        attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("table") {
                table = Some(meta.value()?.parse::<LitStr>()?);
                Ok(())
            } else {
                Err(meta.error("expected `table = \"...\"`"))
            }
        })?;
    }
    let table = table.ok_or_else(|| {
        syn::Error::new_spanned(&input.ident, "name the table: #[model(table = \"posts\")]")
    })?;
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(Model)] needs a struct with named fields",
        ));
    };
    let Fields::Named(fields) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input.ident,
            "#[derive(Model)] needs a struct with named fields",
        ));
    };
    let fields = fields.named.iter().map(|field| {
        let name = field.ident.as_ref().expect("named field");
        let column = name.to_string();
        let column = column.strip_prefix("r#").unwrap_or(&column);
        quote! { #name: row.get(#column)? }
    });
    let ident = &input.ident;
    let (impl_generics, type_generics, where_clause) = input.generics.split_for_impl();
    Ok(quote! {
        impl #impl_generics ::rustclamp::db::Model for #ident #type_generics #where_clause {
            const TABLE: &'static str = #table;

            fn from_row(
                row: &::rustclamp::db::sqlite::Row<'_>,
            ) -> ::rustclamp::db::sqlite::Result<Self> {
                ::core::result::Result::Ok(Self { #(#fields,)* })
            }
        }
    })
}
