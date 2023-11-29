use proc_macro::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::{parse::Parser, parse_macro_input, Data, DeriveInput, Field, Fields, Ident};
use syn::{Attribute, Expr, ExprAssign, ExprLit, ExprPath, Lit, Path, Token};

fn ensure_has_pool_field(data: Data, provider: &str, pool_type: &str) -> () {
    if let Data::Struct(data) = data {
        if let Fields::Named(fields) = data.fields {
            if let None = fields
                .named
                .iter()
                .find(|f| f.ident.is_some() && f.ident.as_ref().unwrap().to_string() == "pool")
            {
                panic!(
                    "#[derive({})] struct needs to implement a pool field of type {}",
                    provider, pool_type
                );
            }
        }
    } else {
        panic!("#[derive({})] is only defined for structs!", provider);
    }
}

fn provider_params(attrs: Vec<Attribute>, default_to: &str, default_from: &str) -> (Ident, Ident) {
    let mut to: Option<Ident> = None;
    let mut from: Option<Ident> = None;
    for attr in attrs {
        if let syn::Meta::List(syn::MetaList { tokens, .. }) = attr.meta {
            let parser = Punctuated::<Expr, Token![,]>::parse_terminated;
            for expr in parser.parse(tokens.into()).unwrap() {
                match expr {
                    Expr::Assign(ExprAssign { left, right, .. }) => match (*left, *right) {
                        (
                            Expr::Path(ExprPath {
                                path:
                                    Path {
                                        segments: left_segments,
                                        ..
                                    },
                                ..
                            }),
                            Expr::Lit(ExprLit {
                                lit: Lit::Str(right),
                                ..
                            }),
                        ) if left_segments.len() == 1 => {
                            match &left_segments.first().unwrap().ident {
                                i if i.to_string() == "To" => {
                                    to = Some(Ident::new(
                                        &right.value(),
                                        proc_macro2::Span::call_site(),
                                    ));
                                }
                                i if i.to_string() == "From" => {
                                    from = Some(Ident::new(
                                        &right.value(),
                                        proc_macro2::Span::call_site(),
                                    ));
                                }
                                _ => {
                                    panic!("Wrong macro 3");
                                }
                            }
                        }
                        (_a, _b) => {
                            panic!("Wrong macro 1");
                        }
                    },
                    _ => {
                        panic!("Wrong macro 2");
                    }
                }
            }
        }
    }
    (
        to.unwrap_or(Ident::new(default_to, proc_macro2::Span::call_site())),
        from.unwrap_or(Ident::new(default_from, proc_macro2::Span::call_site())),
    )
}

/// Macro to implement an IdentityProvider on an struct.
///
/// It will implement the traits ServiceUsersPool and ServiceIdentityProvider with types From and To.
/// It accepts attributes From and To to specify the types for the ServiceIdentityProvider.
/// The attribute is IdentityProvider.
///
/// Note that right now the struct needs to have a pool field of type IdentityManagerPool.
/// This can be added with the macro `identity_provider`.
///
/// # Examples:
///
/// ```ignore
/// #[identity_provider()]
/// #[derive(IdentityProvider, Default)]
/// #[IdentityProvider(From = "Deployment", To = "ServiceIdentity")]
/// struct TestIdentityManager {
///     field1: bool
/// }
/// ```
///
/// # Panics
///
/// Panics if .
#[proc_macro_derive(IdentityProvider, attributes(IdentityProvider))]
pub fn derive_identity_provider(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let DeriveInput {
        ident: name,
        attrs,
        data,
        ..
    } = parse_macro_input!(input);
    ensure_has_pool_field(data, "IdentityProvider", "IdentityManagerPool");
    let (to, from) = provider_params(attrs, "ServiceIdentity", "ServiceLookup");
    // Code to expand
    let expanded = quote! {
        impl ServiceIdentityProvider for #name {
            type From = #from;
            type To = #to;

            fn register_identity(&mut self, identity: Self::To) -> () {
                self.pool.register_identity(identity)
            }

            fn unregister_identity(&mut self, to: &Self::To) -> Option<Self::To> {
                self.pool.unregister_identity(to)
            }

            fn next_identity(&mut self, from: &Self::From) -> Option<(Self::To, bool)> {
                self.pool.next_identity(from)
            }

            fn identity(&self, from: &Self::From) -> Option<&Self::To> {
                self.pool.identity(from)
            }

            fn identities(&self) -> Vec<&Self::To> {
                self.pool.identities()
            }
        }
        impl ServiceUsersPool for #name {
            fn pop(&mut self) -> Option<ServiceUser> {
                self.pool.pop()
            }

            fn push(&mut self, user_credentials_ref: ServiceUser) -> () {
                self.pool.push(user_credentials_ref)
            }

            fn needs_new_credentials(&self) -> bool {
                self.pool.needs_new_credentials()
            }
        }
        impl IdentityManager<#from, #to> for #name {}
    };
    expanded.into()
}

/// Macro to prepares a struct to be used as an IdentityProvider
/// It adds an IdentityManagerPool field (named pool)
///
/// # Examples
///
/// ```ignore
/// #[identity_provider()]
/// struct MyIdManager {
///     field1: i32,
///     field2: bool,
/// }
/// ```
///
/// # Panics
///
/// Panics if it's applied to something that it's not an struct.
#[proc_macro_attribute]
pub fn identity_provider(_args: TokenStream, input: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(input as DeriveInput);
    if let Data::Struct(ref mut data) = input.data {
        if let Fields::Named(ref mut fields) = &mut data.fields {
            let pool_field = Field::parse_named
                .parse2(quote! { pool: IdentityManagerPool })
                .expect("Unable to parse pool field!");
            fields.named.push(pool_field);
        }
    } else {
        panic!("#[identity_provider] is only defined for structs!");
    }

    quote! {
        #input
    }
    .into()
}

#[proc_macro_derive(DeviceIdProvider, attributes(DeviceIdProvider))]
pub fn derive_device_id_provider(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    // Parse the input tokens into a syntax tree.
    let mut _has_pool_field: bool = false;
    let DeriveInput {
        ident: name,
        attrs,
        data,
        ..
    } = parse_macro_input!(input);
    ensure_has_pool_field(data, "DeviceIdProvider", "DeviceIdManagerPool");
    let (to, from) = provider_params(attrs, "ServiceIdentity", "DeviceId");
    // Expand this code
    let expanded = quote! {
        impl DeviceIdProvider for #name {
            type From = #from;
            type To = #to;

            fn register(&mut self, device_id: Self::To) -> () {
                self.pool.register(device_id)
            }

            fn unregister(&mut self, to: &Self::To) -> Option<Self::To> {
                self.pool.unregister(to)
            }

            fn device_id(&self, from: &Self::From) -> Option<&Self::To> {
                self.pool.device_id(from)
            }

            fn device_ids(&self) -> Vec<&Self::To> {
                self.pool.device_ids()
            }

            fn next_device_id(&self, from: &Self::From) -> Option<Self::To> {
                self.pool.next_device_id(from)
            }
        }

        impl DeviceIdManager<#from, #to> for #name {}
    };
    expanded.into()
}

#[proc_macro_attribute]
pub fn device_id_provider(_args: TokenStream, input: TokenStream) -> TokenStream {
    let mut input = parse_macro_input!(input as DeriveInput);
    if let Data::Struct(ref mut data) = input.data {
        if let Fields::Named(ref mut fields) = &mut data.fields {
            let pool_field = Field::parse_named
                .parse2(quote! { pool: DeviceIdManagerPool })
                .expect("Unable to parse pool field!");
            fields.named.push(pool_field);
        }
    } else {
        panic!("#[device_id_provider] is only defined for structs!");
    }

    quote! {
        #input
    }
    .into()
}
