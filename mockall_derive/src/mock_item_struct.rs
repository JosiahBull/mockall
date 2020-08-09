// vim: tw=80
use super::*;

use quote::ToTokens;

use crate::{
    mock_function::MockFunction,
    mock_trait::MockTrait
};

fn phantom_default_inits(generics: &Generics) -> Vec<TokenStream> {
    generics.params
    .iter()
    .enumerate()
    .map(|(count, _param)| {
        let phident = format_ident!("_t{}", count);
        quote!(#phident: ::std::marker::PhantomData)
    }).collect()
}

/// Generate any PhantomData field definitions
fn phantom_fields(generics: &Generics) -> Vec<TokenStream> {
    generics.params
    .iter()
    .enumerate()
    .filter_map(|(count, param)| {
        let phident = format_ident!("_t{}", count);
        match param {
            syn::GenericParam::Lifetime(l) => {
                if !l.bounds.is_empty() {
                    compile_error(l.bounds.span(),
                        "#automock does not yet support lifetime bounds on structs");
                }
                let lifetime = &l.lifetime;
                Some(
                quote!(#phident: ::std::marker::PhantomData<&#lifetime ()>)
                )
            },
            syn::GenericParam::Type(tp) => {
                let ty = &tp.ident;
                Some(
                quote!(#phident: ::std::marker::PhantomData<#ty>)
                )
            },
            syn::GenericParam::Const(_) => {
                compile_error(param.span(),
                    "#automock does not yet support generic constants");
                None
            }
        }
    }).collect()
}

/// A collection of methods defined in one spot
struct Methods(Vec<MockFunction>);

impl Methods {
    fn checkpoints(&self) -> Vec<impl ToTokens> {
        self.0.iter()
            .filter(|meth| !meth.is_static())
            .map(|meth| meth.checkpoint())
            .collect::<Vec<_>>()
    }

    /// Return a fragment of code to initialize struct fields during default()
    fn default_inits(&self) -> Vec<TokenStream> {
        self.0.iter()
            .filter(|meth| !meth.is_static())
            .map(|meth| {
                let name = meth.name();
                let attrs = meth.format_attrs(false);
                quote!(#attrs #name: Default::default())
            }).collect::<Vec<_>>()
    }

    fn field_definitions(&self, modname: &Ident) -> Vec<TokenStream> {
        self.0.iter()
            .filter(|meth| !meth.is_static())
            .map(|meth| meth.field_definition(Some(modname)))
            .collect::<Vec<_>>()
    }

    fn priv_mods(&self) -> Vec<impl ToTokens> {
        self.0.iter()
            .map(|meth| meth.priv_module())
            .collect::<Vec<_>>()
    }
}

pub(crate) struct MockItemStruct {
    attrs: Vec<Attribute>,
    generics: Generics,
    /// Does the original struct have a `new` method?
    has_new: bool,
    /// Inherent methods of the mock struct
    methods: Methods,
    /// Name of the overall module that holds all of the mock stuff
    modname: Ident,
    name: Ident,
    /// Is this a whole MockStruct or just a substructure for a trait impl?
    traits: Vec<MockTrait>,
    vis: Visibility,
}

impl MockItemStruct {
    fn new_method(&self) -> impl ToTokens {
        if self.has_new {
            TokenStream::new()
        } else {
            quote!(
                /// Create a new mock object with no expectations.
                ///
                /// This method will not be generated if the real struct
                /// already has a `new` method.  However, it *will* be
                /// generated if the struct implements a trait with a `new`
                /// method.  The trait's `new` method can still be called
                /// like `<MockX as TraitY>::new`
                pub fn new() -> Self {
                    Self::default()
                }
            )
        }
    }

    fn phantom_default_inits(&self) -> Vec<TokenStream> {
        phantom_default_inits(&self.generics)
    }

    fn phantom_fields(&self) -> Vec<TokenStream> {
        phantom_fields(&self.generics)
    }
}

impl From<MockableStruct> for MockItemStruct {
    fn from(mockable: MockableStruct) -> MockItemStruct {
        let modname = gen_mod_ident(&mockable.name, None);
        let generics = mockable.generics.clone();
        let struct_name = &mockable.name;
        let vis = mockable.vis;
        let has_new = mockable.methods.iter()
            .any(|meth| meth.sig.ident == "new") ||
            mockable.traits.iter()
            .any(|trait_|
                trait_.items.iter()
                    .any(|ti| if let TraitItem::Method(tim) = ti {
                            tim.sig.ident == "new"
                        } else {
                            false
                        }
                    )
            );
        let methods = Methods(mockable.methods.into_iter()
            .map(|meth|
                mock_function::Builder::new(&meth.sig, &meth.vis)
                    .attrs(&meth.attrs)
                    .struct_(&struct_name)
                    .struct_generics(&generics)
                    .levels(2)
                    .call_levels(0)
                    .build()
            ).collect::<Vec<_>>());
        let structname = &mockable.name;
        let traits = mockable.traits.into_iter()
            .map(|t| MockTrait::new(structname, &generics, t, &vis))
            .collect();

        MockItemStruct {
            attrs: mockable.attrs,
            generics,
            has_new,
            methods,
            modname,
            name: mockable.name,
            traits,
            vis
        }
    }
}

impl ToTokens for MockItemStruct {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let attrs = &self.attrs;
        let struct_name = &self.name;
        let (ig, tg, wc) = self.generics.split_for_impl();
        let modname = &self.modname;
        let calls = self.methods.0.iter()
            .map(|meth| meth.call(Some(modname)))
            .collect::<Vec<_>>();
        let contexts = self.methods.0.iter()
            .filter(|meth| meth.is_static())
            .map(|meth| meth.context_fn(Some(modname)))
            .collect::<Vec<_>>();
        let expects = self.methods.0.iter()
            .filter(|meth| !meth.is_static())
            .map(|meth| meth.expect(modname))
            .collect::<Vec<_>>();
        let method_checkpoints = self.methods.checkpoints();
        let new_method = self.new_method();
        let priv_mods = self.methods.priv_mods();
        let substructs = self.traits.iter()
            .map(|trait_| {
                MockItemTraitImpl {
                    attrs: trait_.attrs.clone(),
                    generics: self.generics.clone(),
                    fieldname: format_ident!("{}_expectations", trait_.name()),
                    methods: Methods(trait_.methods.clone()),
                    modname: format_ident!("{}_{}", &self.modname, trait_.name()),
                    name: format_ident!("{}_{}", &self.name, trait_.name()),
                }
            }).collect::<Vec<_>>();
        let substruct_expectations = self.traits.iter()
            .map(|trait_| format_ident!("{}_expectations", trait_.name()))
            .collect::<Vec<_>>();
        let mut field_definitions = substructs.iter()
            .map(|ss| {
                let fieldname = &ss.fieldname;
                let tyname = &ss.name;
                quote!(#fieldname: #tyname #tg)
            }).collect::<Vec<_>>();
        field_definitions.extend(self.methods.field_definitions(&modname));
        field_definitions.extend(self.phantom_fields());
        let mut default_inits = substructs.iter()
            .map(|ss| {
                let fieldname = &ss.fieldname;
                quote!(#fieldname: Default::default())
            }).collect::<Vec<_>>();
        default_inits.extend(self.methods.default_inits());
        default_inits.extend(self.phantom_default_inits());
        let trait_impls = self.traits.iter()
            .map(|ss| {
                let modname = format_ident!("{}_{}", &self.modname, ss.name());
                ss.trait_impl(&modname)
            }).collect::<Vec<_>>();
        let vis = &self.vis;
        quote!(
            #[allow(non_snake_case)]
            #[allow(missing_docs)]
            pub mod #modname {
                use super::*;
                #(#priv_mods)*
            }
            #[allow(non_camel_case_types)]
            #[allow(non_snake_case)]
            #[allow(missing_docs)]
            #(#attrs)*
            #vis struct #struct_name #ig #wc
            {
                #(#field_definitions),*
            }
            impl #ig ::std::default::Default for #struct_name #tg #wc {
                fn default() -> Self {
                    Self {
                        #(#default_inits),*
                    }
                }
            }
            #(#substructs)*
            impl #ig #struct_name #tg #wc {
                #(#calls)*
                #(#contexts)*
                #(#expects)*
                /// Validate that all current expectations for all methods have
                /// been satisfied, and discard them.
                pub fn checkpoint(&mut self) {
                    #(self.#substruct_expectations.checkpoint();)*
                    #(#method_checkpoints)*
                }
                #new_method
            }
            #(#trait_impls)*
        ).to_tokens(tokens);
    }
}

pub(crate) struct MockItemTraitImpl {
    attrs: Vec<Attribute>,
    generics: Generics,
    /// Inherent methods of the mock struct
    methods: Methods,
    /// Name of the overall module that holds all of the mock stuff
    modname: Ident,
    name: Ident,
    /// Name of the field of this type in the parent's structure
    fieldname: Ident,
}

impl MockItemTraitImpl {
    fn phantom_default_inits(&self) -> Vec<TokenStream> {
        phantom_default_inits(&self.generics)
    }

    fn phantom_fields(&self) -> Vec<TokenStream> {
        phantom_fields(&self.generics)
    }
}

impl ToTokens for MockItemTraitImpl {
    fn to_tokens(&self, tokens: &mut TokenStream) {
        let attrs_nodocs = format_attrs(&self.attrs, false);
        let struct_name = &self.name;
        let (ig, tg, wc) = self.generics.split_for_impl();
        let modname = &self.modname;
        let method_checkpoints = self.methods.checkpoints();
        let mut default_inits = self.methods.default_inits();
        default_inits.extend(self.phantom_default_inits());
        let mut field_definitions = self.methods.field_definitions(&modname);
        field_definitions.extend(self.phantom_fields());
        let priv_mods = self.methods.priv_mods();
        quote!(
            #[allow(non_snake_case)]
            #[allow(missing_docs)]
            pub mod #modname {
                use super::*;
                #(#priv_mods)*
            }
            #[allow(non_camel_case_types)]
            #[allow(non_snake_case)]
            #[allow(missing_docs)]
            #attrs_nodocs
            struct #struct_name #ig #wc
            {
                #(#field_definitions),*
            }
            impl #ig ::std::default::Default for #struct_name #tg #wc {
                fn default() -> Self {
                    Self {
                        #(#default_inits),*
                    }
                }
            }
            impl #ig #struct_name #tg #wc {
                /// Validate that all current expectations for all methods have
                /// been satisfied, and discard them.
                pub fn checkpoint(&mut self) {
                    #(#method_checkpoints)*
                }
            }
        ).to_tokens(tokens);
    }
}
