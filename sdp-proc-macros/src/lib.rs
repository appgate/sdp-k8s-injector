use proc_macro::TokenStream;
use quote::quote;
use syn::Meta::NameValue;
use syn::NestedMeta::Meta;
use syn::{parse::Parser, parse_macro_input, Data, DeriveInput, Field, Fields, Ident, NestedMeta};

struct IdentityProviderParams {
    from: Ident,
    to: Ident,
}

/// Macro to implement an IdentityProvider on an struct.
///
/// It will implement the traits ServiceCredentialsPool and ServiceIdentityProvider with types From and To.
/// It accepts attributes From and To to specify the types for the ServiceIdentityProvider.
/// The attribute is IdentityProvider.
///
/// Note that right the struct needs to have a pool field of type IdentityManagerPool.
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
    // Parse the input tokens into a syntax tree.
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let _attrs = &input.attrs;
    let mut _has_pool_field = false;
    if let Data::Struct(data) = input.data {
        if let Fields::Named(fields) = data.fields {
            if let Some(_field) = fields
                .named
                .iter()
                .find(|f| f.ident.is_some() && f.ident.as_ref().unwrap().to_string() == "pool")
            {
                _has_pool_field = true;
            }
        }
    } else {
        panic!("#[derive(IdentityProvider)] is only defined for structs!");
    }

    let ms: Vec<NestedMeta> = input
        .attrs
        .iter()
        .flat_map(|a| match a.parse_meta() {
            Ok(syn::Meta::List(meta)) => meta.nested.into_iter().collect(),
            _ => {
                vec![]
            }
        })
        .collect();

    let mut identity_provider_params = IdentityProviderParams {
        from: Ident::new("Deployment", proc_macro2::Span::call_site()),
        to: Ident::new("ServiceIdentity", proc_macro2::Span::call_site()),
    };
    for m in ms {
        match m {
            Meta(NameValue(nv)) => {
                let left = if let Some(s) = nv.path.segments.into_iter().last() {
                    s.ident
                } else {
                    panic!("Use IdentityProviderParams(...)");
                };
                let right = if let syn::Lit::Str(lit) = nv.lit {
                    Ident::new(lit.value().as_str(), proc_macro2::Span::call_site())
                } else {
                    panic!("Use IdentityProviderParams(...)");
                };
                match left.to_string().as_str() {
                    "TO" => {
                        identity_provider_params.to = right;
                    }
                    "FROM" => {
                        identity_provider_params.from = right;
                    }
                    _ => (),
                }
            }
            _ => {
                panic!("Use IdentityProviderParams(...)");
            }
        };
    }

    // We need a pool field!
    if !_has_pool_field {
        panic!("#[derive(IdentityProvider)] struct needs to implement a pool field of type IdentityManagerPool");
    }

    // Get the IdentityProvider parameters
    let to = identity_provider_params.to;
    let from = identity_provider_params.from;

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

            fn next_identity(&mut self, from: &Self::From) -> Option<Self::To> {
                self.pool.next_identity(from)
            }

            fn identity(&self, from: &Self::From) -> Option<&Self::To> {
                self.pool.identity(from)
            }

            fn identities(&self) -> Vec<&Self::To> {
                self.pool.identities()
            }
        }
        impl ServiceCredentialsPool for #name {
            fn pop(&mut self) -> Option<ServiceCredentialsRef> {
                self.pool.pop()
            }

            fn push(&mut self, user_credentials_ref: ServiceCredentialsRef) -> () {
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
    let input = parse_macro_input!(input as DeriveInput);
    let name = &input.ident;
    let _attrs = &input.attrs;
    let mut _has_pool_field = false;
    if let Data::Struct(data) = input.data {
        if let Fields::Named(fields) = data.fields {
            if let Some(_field) = fields
                .named
                .iter()
                .find(|f| f.ident.is_some() && f.ident.as_ref().unwrap().to_string() == "pool")
            {
                _has_pool_field = true;
            }
        }
    } else {
        panic!("#[derive(DeviceIdProider)] is only defined for structs!");
    }

    let ms: Vec<NestedMeta> = input
        .attrs
        .iter()
        .flat_map(|a| match a.parse_meta() {
            Ok(syn::Meta::List(meta)) => meta.nested.into_iter().collect(),
            _ => {
                vec![]
            }
        })
        .collect();

    let mut device_id_params = IdentityProviderParams {
        from: Ident::new("ServiceIdentity", proc_macro2::Span::call_site()),
        to: Ident::new("DeviceId", proc_macro2::Span::call_site()),
    };
    for m in ms {
        match m {
            Meta(NameValue(nv)) => {
                let left = if let Some(s) = nv.path.segments.into_iter().last() {
                    s.ident
                } else {
                    panic!("Use DeviceIdProviderParams(...)");
                };
                let right = if let syn::Lit::Str(lit) = nv.lit {
                    Ident::new(lit.value().as_str(), proc_macro2::Span::call_site())
                } else {
                    panic!("Use DeviceIdProviderParams(...)");
                };
                match left.to_string().as_str() {
                    "TO" => {
                        device_id_params.to = right;
                    }
                    "FROM" => {
                        device_id_params.from = right;
                    }
                    _ => (),
                }
            }
            _ => {
                panic!("Use DeviceIdProviderParams(...)");
            }
        };
    }

    // Requires pool field
    if !_has_pool_field {
        panic!("#[derive(DeviceIdProvider)] struct needs to implement a 'pool' field of type DeviceIdManagerPool");
    }

    // Get the parameters
    let to = device_id_params.to;
    let from = device_id_params.from;

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
        }
        impl DeviceIdPool for #name {
            fn pop(&mut self) -> Option<String> {
                self.pool.pop()
            }

            fn push(&mut self, device_id: String) -> () {
                self.pool.push(device_id)
            }

            fn needs_new_device_id(&self) -> bool {
                self.pool.needs_new_device_id()
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
