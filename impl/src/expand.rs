use crate::ast::{Enum, Field, Input, Struct};
use proc_macro2::TokenStream;
use quote::{format_ident, quote, quote_spanned, ToTokens};
use syn::spanned::Spanned;
use syn::{DeriveInput, Member, PathArguments, Result, Type};

pub fn derive(node: &DeriveInput) -> Result<TokenStream> {
    let input = Input::from_syn(node)?;
    input.validate()?;
    Ok(match input {
        Input::Struct(input) => impl_struct(input),
        Input::Enum(input) => impl_enum(input),
    })
}

fn impl_struct(input: Struct) -> TokenStream {
    let ty = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let source_body = if input.attrs.transparent.is_some() {
        let only_field = &input.fields[0].member;
        Some(quote! {
            core_error::Error::source(self.#only_field.as_dyn_error())
        })
    } else if let Some(source_field) = input.source_field() {
        let source = &source_field.member;
        let asref = if type_is_option(source_field.ty) {
            Some(quote_spanned!(source.span()=> .as_ref()?))
        } else {
            None
        };
        let dyn_error = quote_spanned!(source.span()=> self.#source #asref.as_dyn_error());
        Some(quote! {
            std::option::Option::Some(#dyn_error)
        })
    } else {
        None
    };
    let source_method = source_body.map(|body| {
        quote! {
            fn source(&self) -> std::option::Option<&(dyn core_error::Error + 'static)> {
                use thiserror::private::AsDynError;
                #body
            }
        }
    });

    let backtrace_method = input.backtrace_field().map(|backtrace_field| {
        let backtrace = &backtrace_field.member;
        let body = if let Some(source_field) = input.source_field() {
            let source = &source_field.member;
            let source_backtrace = if type_is_option(source_field.ty) {
                quote_spanned! {source.span()=>
                    self.#source.as_ref().and_then(|source| source.as_dyn_error().backtrace())
                }
            } else {
                quote_spanned! {source.span()=>
                    self.#source.as_dyn_error().backtrace()
                }
            };
            let combinator = if type_is_option(backtrace_field.ty) {
                quote! {
                    #source_backtrace.or(self.#backtrace.as_ref())
                }
            } else {
                quote! {
                    std::option::Option::Some(#source_backtrace.unwrap_or(&self.#backtrace))
                }
            };
            quote! {
                use thiserror::private::AsDynError;
                #combinator
            }
        } else if type_is_option(backtrace_field.ty) {
            quote! {
                self.#backtrace.as_ref()
            }
        } else {
            quote! {
                std::option::Option::Some(&self.#backtrace)
            }
        };
        quote! {
            fn backtrace(&self) -> std::option::Option<&std::backtrace::Backtrace> {
                #body
            }
        }
    });

    let display_body = if input.attrs.transparent.is_some() {
        let only_field = &input.fields[0].member;
        Some(quote! {
            core::fmt::Display::fmt(&self.#only_field, __formatter)
        })
    } else if let Some(display) = &input.attrs.display {
        let use_as_display = if display.has_bonus_display {
            {
                #[cfg(feature = "std")]
                Some(quote! {
                    #[allow(unused_imports)]
                    use thiserror::private::{DisplayAsDisplay, PathAsDisplay};
                })
            }

            #[cfg(not(feature = "std"))]
            Some(quote! {
                #[allow(unused_imports)]
                use thiserror::private::DisplayAsDisplay;
            })
        } else {
            None
        };
        let pat = fields_pat(&input.fields);
        Some(quote! {
            #use_as_display
            #[allow(unused_variables)]
            let Self #pat = self;
            #display
        })
    } else {
        None
    };
    let display_impl = display_body.map(|body| {
        quote! {
            impl #impl_generics core::fmt::Display for #ty #ty_generics #where_clause {
                fn fmt(&self, __formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                    #body
                }
            }
        }
    });

    let from_impl = input.from_field().map(|from_field| {
        let backtrace_field = input.backtrace_field();
        let from = from_field.ty;
        let body = from_initializer(from_field, backtrace_field);
        quote! {
            impl #impl_generics std::convert::From<#from> for #ty #ty_generics #where_clause {
                fn from(source: #from) -> Self {
                    #ty #body
                }
            }
        }
    });

    quote! {
        impl #impl_generics core_error::Error for #ty #ty_generics #where_clause {
            #source_method
            #backtrace_method
        }
        #display_impl
        #from_impl
    }
}

fn impl_enum(input: Enum) -> TokenStream {
    let ty = &input.ident;
    let (impl_generics, ty_generics, where_clause) = input.generics.split_for_impl();

    let source_method = if input.has_source() {
        let arms = input.variants.iter().map(|variant| {
            let ident = &variant.ident;
            if variant.attrs.transparent.is_some() {
                let only_field = &variant.fields[0].member;
                let source = quote!(core_error::Error::source(transparent.as_dyn_error()));
                quote! {
                    #ty::#ident {#only_field: transparent} => #source,
                }
            } else if let Some(source_field) = variant.source_field() {
                let source = &source_field.member;
                let asref = if type_is_option(source_field.ty) {
                    Some(quote_spanned!(source.span()=> .as_ref()?))
                } else {
                    None
                };
                let dyn_error = quote_spanned!(source.span()=> source #asref.as_dyn_error());
                quote! {
                    #ty::#ident {#source: source, ..} => std::option::Option::Some(#dyn_error),
                }
            } else {
                quote! {
                    #ty::#ident {..} => std::option::Option::None,
                }
            }
        });
        Some(quote! {
            fn source(&self) -> std::option::Option<&(dyn core_error::Error + 'static)> {
                use thiserror::private::AsDynError;
                match self {
                    #(#arms)*
                }
            }
        })
    } else {
        None
    };

    let backtrace_method = if input.has_backtrace() {
        let arms = input.variants.iter().map(|variant| {
            let ident = &variant.ident;
            match (variant.backtrace_field(), variant.source_field()) {
                (Some(backtrace_field), Some(source_field))
                    if backtrace_field.attrs.backtrace.is_none() =>
                {
                    let backtrace = &backtrace_field.member;
                    let source = &source_field.member;
                    let source_backtrace = if type_is_option(source_field.ty) {
                        quote_spanned! {source.span()=>
                            source.as_ref().and_then(|source| source.as_dyn_error().backtrace())
                        }
                    } else {
                        quote_spanned! {source.span()=>
                            source.as_dyn_error().backtrace()
                        }
                    };
                    let combinator = if type_is_option(backtrace_field.ty) {
                        quote! {
                            #source_backtrace.or(backtrace.as_ref())
                        }
                    } else {
                        quote! {
                            std::option::Option::Some(#source_backtrace.unwrap_or(backtrace))
                        }
                    };
                    quote! {
                        #ty::#ident {
                            #backtrace: backtrace,
                            #source: source,
                            ..
                        } => {
                            use thiserror::private::AsDynError;
                            #combinator
                        }
                    }
                }
                (Some(backtrace_field), _) => {
                    let backtrace = &backtrace_field.member;
                    let body = if type_is_option(backtrace_field.ty) {
                        quote!(backtrace.as_ref())
                    } else {
                        quote!(std::option::Option::Some(backtrace))
                    };
                    quote! {
                        #ty::#ident {#backtrace: backtrace, ..} => #body,
                    }
                }
                (None, _) => quote! {
                    #ty::#ident {..} => std::option::Option::None,
                },
            }
        });
        Some(quote! {
            fn backtrace(&self) -> std::option::Option<&std::backtrace::Backtrace> {
                match self {
                    #(#arms)*
                }
            }
        })
    } else {
        None
    };

    let display_impl = if input.has_display() {
        let use_as_display = if input.variants.iter().any(|v| {
            v.attrs
                .display
                .as_ref()
                .map_or(false, |display| display.has_bonus_display)
        }) {
            {
                #[cfg(feature = "std")]
                Some(quote! {
                    #[allow(unused_imports)]
                    use thiserror::private::{DisplayAsDisplay, PathAsDisplay};
                })
            }

            #[cfg(not(feature = "std"))]
            Some(quote! {
                #[allow(unused_imports)]
                use thiserror::private::DisplayAsDisplay;
            })
        } else {
            None
        };
        let void_deref = if input.variants.is_empty() {
            Some(quote!(*))
        } else {
            None
        };
        let arms = input.variants.iter().map(|variant| {
            let display = match &variant.attrs.display {
                Some(display) => display.to_token_stream(),
                None => {
                    let only_field = match &variant.fields[0].member {
                        Member::Named(ident) => ident.clone(),
                        Member::Unnamed(index) => format_ident!("_{}", index),
                    };
                    quote!(core::fmt::Display::fmt(#only_field, __formatter))
                }
            };
            let ident = &variant.ident;
            let pat = fields_pat(&variant.fields);
            quote! {
                #ty::#ident #pat => #display
            }
        });
        Some(quote! {
            impl #impl_generics core::fmt::Display for #ty #ty_generics #where_clause {
                fn fmt(&self, __formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                    #use_as_display
                    #[allow(unused_variables)]
                    match #void_deref self {
                        #(#arms,)*
                    }
                }
            }
        })
    } else {
        None
    };

    let from_impls = input.variants.iter().filter_map(|variant| {
        let from_field = variant.from_field()?;
        let backtrace_field = variant.backtrace_field();
        let variant = &variant.ident;
        let from = from_field.ty;
        let body = from_initializer(from_field, backtrace_field);
        Some(quote! {
            impl #impl_generics std::convert::From<#from> for #ty #ty_generics #where_clause {
                fn from(source: #from) -> Self {
                    #ty::#variant #body
                }
            }
        })
    });

    quote! {
        impl #impl_generics core_error::Error for #ty #ty_generics #where_clause {
            #source_method
            #backtrace_method
        }
        #display_impl
        #(#from_impls)*
    }
}

fn fields_pat(fields: &[Field]) -> TokenStream {
    let mut members = fields.iter().map(|field| &field.member).peekable();
    match members.peek() {
        Some(Member::Named(_)) => quote!({ #(#members),* }),
        Some(Member::Unnamed(_)) => {
            let vars = members.map(|member| match member {
                Member::Unnamed(member) => format_ident!("_{}", member),
                Member::Named(_) => unreachable!(),
            });
            quote!((#(#vars),*))
        }
        None => quote!({}),
    }
}

fn from_initializer(from_field: &Field, backtrace_field: Option<&Field>) -> TokenStream {
    let from_member = &from_field.member;
    let backtrace = backtrace_field.map(|backtrace_field| {
        let backtrace_member = &backtrace_field.member;
        if type_is_option(backtrace_field.ty) {
            quote! {
                #backtrace_member: std::option::Option::Some(std::backtrace::Backtrace::capture()),
            }
        } else {
            quote! {
                #backtrace_member: std::backtrace::Backtrace::capture(),
            }
        }
    });
    quote!({
        #from_member: source,
        #backtrace
    })
}

fn type_is_option(ty: &Type) -> bool {
    let path = match ty {
        Type::Path(ty) => &ty.path,
        _ => return false,
    };

    let last = path.segments.last().unwrap();
    if last.ident != "Option" {
        return false;
    }

    match &last.arguments {
        PathArguments::AngleBracketed(bracketed) => bracketed.args.len() == 1,
        _ => false,
    }
}
