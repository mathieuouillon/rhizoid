//! `#[derive(RootObject)]` and `#[derive(Row)]` for **rhizoid-io**.
//!
//! Use them through `rhizoid_io` (feature `derive`, on by default), which
//! re-exports them next to the traits of the same names:
//!
//! ```ignore
//! use rhizoid_io::{RootObject, Row, objects::Named};
//!
//! #[derive(RootObject)]
//! #[root(class = "UserEvent", version = 3, rename_all = "root")]
//! struct UserEvent {
//!     #[root(base)]
//!     named: Named,
//!     run: i32,                       // int fRun
//!     p: [f64; 4],                    // double fP[4]
//!     n: i32,                         // int fN
//!     #[root(count = n)]
//!     pt: Vec<f32>,                   // float* fPt; //[fN]
//!     tag: String,                    // TString fTag
//!     #[root(std_string)]
//!     label: String,                  // std::string fLabel
//!     ids: Vec<i32>,                  // std::vector<int> fIds
//!     #[root(alias = "fOldName")]
//!     renamed: f64,                   // read from fRenamed or fOldName
//! }
//!
//! #[derive(Row)]
//! struct Muons {
//!     #[row(rename = "nMuon")]
//!     n: u32,
//!     #[row(rename = "Muon_pt")]
//!     pt: Vec<f32>,
//! }
//! ```
//!
//! Every mistake in the attributes is reported in one build, at its span.

use proc_macro::TokenStream;
use proc_macro2::{Span, TokenStream as TokenStream2};
use quote::{format_ident, quote};
use syn::spanned::Spanned;
use syn::{
    Data, DeriveInput, Fields, GenericArgument, Ident, LitInt, LitStr, PathArguments, Type,
    parse_macro_input,
};

/// Collects every error of one expansion.
#[derive(Default)]
struct Errors(Vec<syn::Error>);

impl Errors {
    fn push(&mut self, span: Span, message: impl std::fmt::Display) {
        self.0.push(syn::Error::new(span, message));
    }

    fn finish(self) -> Result<(), TokenStream2> {
        let mut all = self.0.into_iter();
        match all.next() {
            None => Ok(()),
            Some(mut first) => {
                for e in all {
                    first.combine(e);
                }
                Err(first.to_compile_error())
            }
        }
    }
}

/// Consumes the `= value` of an unknown key, so that one mistake gives one
/// error.
fn skip_value(m: &syn::meta::ParseNestedMeta<'_>) -> syn::Result<()> {
    if m.input.peek(syn::Token![=]) {
        let _: proc_macro2::TokenTree = m.value()?.parse()?;
    }
    Ok(())
}

/// `run` → `fRun`, `event_number` → `fEventNumber`.
fn root_name(ident: &str) -> String {
    let mut out = String::from("f");
    for part in ident
        .trim_start_matches("r#")
        .split('_')
        .filter(|p| !p.is_empty())
    {
        let mut chars = part.chars();
        if let Some(c) = chars.next() {
            out.extend(c.to_uppercase());
            out.push_str(chars.as_str());
        }
    }
    out
}

/// The crate path the generated code names: `rhizoid_io`, or `crate = …`.
fn crate_tokens(path: Option<&LitStr>) -> TokenStream2 {
    if let Some(p) = path {
        let path: syn::Path = p.parse().unwrap_or_else(|_| syn::parse_quote!(rhizoid_io));
        quote!(use #path as _rhizoid;)
    } else {
        quote!(
            #[allow(unused_extern_crates)]
            extern crate rhizoid_io as _rhizoid;
        )
    }
}

/// The item type of `Vec<T>`.
fn vec_item(ty: &Type) -> Option<&Type> {
    let Type::Path(p) = ty else { return None };
    let last = p.path.segments.last()?;
    if last.ident != "Vec" {
        return None;
    }
    match &last.arguments {
        PathArguments::AngleBracketed(a) => a.args.iter().find_map(|g| match g {
            GenericArgument::Type(t) => Some(t),
            _ => None,
        }),
        _ => None,
    }
}

/// What a field of a `RootObject` is.
enum Role {
    Base,
    Member,
    /// An `int` counting another member's `//[count]` array.
    Counter,
    /// A `//[count]` array of the item type.
    Counted {
        count: String,
        item: Box<Type>,
    },
    StdString,
    Skip,
}

struct RootField {
    ident: Ident,
    ty: Type,
    /// The on-disk name (for a base: its class, from the type).
    name: String,
    aliases: Vec<String>,
    role: Role,
}

/// Derives `RootObject` (with `RootClass`, `FromDyn`, `ToDyn`, `RootType`
/// and `VecItem`) for a struct with named fields.
///
/// Container attributes, `#[root(…)]`: `class = "Name"` (default: the
/// struct's name), `version = N` (1 to `0x3FFF`, default 1),
/// `rename_all = "root"` (`run` → `fRun`), `crate = "path"`.
///
/// Field attributes: `base` (a base class: `Object`, `Named`, or another
/// derived class), `rename = "fX"`, `alias = "fOld"` (read also from that
/// name; repeatable), `count = field` (a `Vec` of numbers stored as a
/// `//[count]` array, counted by an earlier `i32` field), `std_string`
/// (a `String` stored as `std::string` rather than `TString`), `skip`
/// (not stored; `Default` when read).
#[proc_macro_derive(RootObject, attributes(root))]
pub fn derive_root_object(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match root_object(&input) {
        Ok(t) | Err(t) => t.into(),
    }
}

#[expect(clippy::too_many_lines, reason = "one generator, read top to bottom")]
fn root_object(input: &DeriveInput) -> Result<TokenStream2, TokenStream2> {
    let mut errors = Errors::default();
    let ident = &input.ident;
    if !input.generics.params.is_empty() {
        errors.push(
            input.generics.span(),
            "a ROOT class cannot be generic: give each instantiation its own struct",
        );
    }
    let Data::Struct(data) = &input.data else {
        errors.push(ident.span(), "#[derive(RootObject)] needs a struct");
        errors.finish()?;
        return Err(TokenStream2::new());
    };
    let Fields::Named(named) = &data.fields else {
        errors.push(ident.span(), "#[derive(RootObject)] needs named fields");
        errors.finish()?;
        return Err(TokenStream2::new());
    };

    // Container attributes.
    let (mut class, mut version, mut rename_all, mut krate) =
        (ident.to_string(), 1_i64, false, None::<LitStr>);
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("root")) {
        let parsed = attr.parse_nested_meta(|m| {
            if m.path.is_ident("class") {
                class = m.value()?.parse::<LitStr>()?.value();
            } else if m.path.is_ident("version") {
                let lit: LitInt = m.value()?.parse()?;
                version = lit.base10_parse()?;
                if !(1..=0x3FFF).contains(&version) {
                    errors.push(lit.span(), "a class version is 1 to 0x3FFF (16383)");
                }
            } else if m.path.is_ident("rename_all") {
                let lit: LitStr = m.value()?.parse()?;
                match lit.value().as_str() {
                    "root" => rename_all = true,
                    "none" => rename_all = false,
                    _ => errors.push(
                        lit.span(),
                        "rename_all is \"root\" (fCamelCase) or \"none\"",
                    ),
                }
            } else if m.path.is_ident("crate") {
                krate = Some(m.value()?.parse()?);
            } else {
                errors.push(
                    m.path.span(),
                    "unknown attribute: expected class, version, rename_all or crate",
                );
                skip_value(&m)?;
            }
            Ok(())
        });
        if let Err(e) = parsed {
            errors.0.push(e);
        }
    }

    // Fields.
    let mut fields: Vec<RootField> = Vec::new();
    for f in &named.named {
        let Some(fident) = f.ident.clone() else {
            continue;
        };
        let mut field = RootField {
            name: if rename_all {
                root_name(&fident.to_string())
            } else {
                fident.to_string()
            },
            ident: fident,
            ty: f.ty.clone(),
            aliases: Vec::new(),
            role: Role::Member,
        };
        for attr in f.attrs.iter().filter(|a| a.path().is_ident("root")) {
            let parsed = attr.parse_nested_meta(|m| {
                if m.path.is_ident("base") {
                    field.role = Role::Base;
                } else if m.path.is_ident("rename") {
                    field.name = m.value()?.parse::<LitStr>()?.value();
                } else if m.path.is_ident("alias") {
                    field.aliases.push(m.value()?.parse::<LitStr>()?.value());
                } else if m.path.is_ident("count") {
                    let counter: Ident = m.value()?.parse()?;
                    match vec_item(&f.ty) {
                        Some(item) => {
                            field.role = Role::Counted {
                                count: counter.to_string(),
                                item: Box::new(item.clone()),
                            };
                        }
                        None => errors.push(f.ty.span(), "a `count` member must be a `Vec` of numbers"),
                    }
                } else if m.path.is_ident("std_string") {
                    field.role = Role::StdString;
                } else if m.path.is_ident("skip") {
                    field.role = Role::Skip;
                } else {
                    errors.push(
                        m.path.span(),
                        "unknown attribute: expected base, rename, alias, count, std_string or skip",
                    );
                    skip_value(&m)?;
                }
                Ok(())
            });
            if let Err(e) = parsed {
                errors.0.push(e);
            }
        }
        fields.push(field);
    }

    // `count` names an earlier member, which becomes a counter.
    let counts: Vec<(usize, String, Span)> = fields
        .iter()
        .enumerate()
        .filter_map(|(i, f)| match &f.role {
            Role::Counted { count, .. } => Some((i, count.clone(), f.ident.span())),
            _ => None,
        })
        .collect();
    let mut count_names: Vec<(usize, String)> = Vec::new();
    for (i, count, span) in counts {
        match fields.iter().position(|f| f.ident == count) {
            Some(j) if j < i && matches!(fields[j].role, Role::Member | Role::Counter) => {
                fields[j].role = Role::Counter;
                count_names.push((i, fields[j].name.clone()));
            }
            Some(_) => errors.push(
                span,
                format!("`count = {count}` must name an earlier `i32` member"),
            ),
            None => errors.push(
                span,
                format!("`count = {count}`: no member named `{count}`"),
            ),
        }
    }

    // On-disk names are unique.
    let mut seen: Vec<&str> = Vec::new();
    for f in fields
        .iter()
        .filter(|f| !matches!(f.role, Role::Base | Role::Skip))
    {
        if seen.contains(&f.name.as_str()) {
            errors.push(
                f.ident.span(),
                format!("two members are stored as `{}`", f.name),
            );
        }
        seen.push(&f.name);
    }
    errors.finish()?;

    let version = i16::try_from(version).unwrap_or(1);
    let krate = crate_tokens(krate.as_ref());
    let count_of = |i: usize| {
        count_names
            .iter()
            .find(|(j, _)| *j == i)
            .map(|(_, n)| n.clone())
    };

    // The description: bases first, then the members.
    let mut elements = Vec::new();
    let mut base_infos = Vec::new();
    let mut deps = Vec::new();
    let mut tobject = vec![quote!(false)];
    for f in fields.iter().filter(|f| matches!(f.role, Role::Base)) {
        let ty = &f.ty;
        elements.push(quote!(elements.push(_p::base_element::<#ty>());));
        base_infos.push(quote!(bases.push(<#ty as _p::RootObject>::streamer_info());));
        deps.push(quote!(
            out.push(<#ty as _p::RootObject>::streamer_info());
            <#ty as _p::RootObject>::__dependencies(out);
        ));
        tobject.push(quote!(<#ty as _p::RootObject>::INHERITS_TOBJECT));
    }
    for (i, f) in fields.iter().enumerate() {
        let (ty, name) = (&f.ty, &f.name);
        match &f.role {
            Role::Base | Role::Skip => {}
            Role::Member => {
                elements.push(quote!(elements.push(<#ty as _p::RootType>::__element(#name));));
                deps.push(quote!(<#ty as _p::RootType>::__dependencies(out);));
            }
            Role::Counter => {
                elements.push(quote!(elements.push(_p::counter_element::<#ty>(#name));));
            }
            Role::Counted { item, .. } => {
                let count = count_of(i).unwrap_or_default();
                elements.push(quote!(elements.push(
                    _p::var_array_element::<#item>(#name, #count, #class, #version)
                );));
            }
            Role::StdString => {
                elements.push(quote!(elements.push(_p::std_string_element(#name));));
            }
        }
    }

    // Writing: a DynObject member by member, the counters from the arrays.
    let mut to_dyn = Vec::new();
    for (i, f) in fields.iter().enumerate() {
        let (fi, name) = (&f.ident, &f.name);
        match &f.role {
            Role::Base => to_dyn.push(quote!(_p::push_base(&mut o, _p::ToDyn::to_dyn(&self.#fi));)),
            Role::Member | Role::StdString => {
                to_dyn.push(quote!(o.set(#name, _p::RootType::__to_value(&self.#fi));));
            }
            Role::Counter => {
                let array = fields
                    .iter()
                    .enumerate()
                    .find(|(j, _)| count_of(*j).as_deref() == Some(name.as_str()))
                    .map(|(_, a)| a.ident.clone());
                match array {
                    Some(a) => to_dyn.push(quote!(o.set(#name, _p::count_of(self.#a.len()));)),
                    None => to_dyn.push(quote!(o.set(#name, _p::RootType::__to_value(&self.#fi));)),
                }
            }
            Role::Counted { .. } => {
                let _ = i;
                to_dyn.push(quote!(o.set(#name, _p::scalars_value(&self.#fi));));
            }
            Role::Skip => {}
        }
        if let Role::StdString = f.role {
            let ty = &f.ty;
            to_dyn.push(quote!(let _: fn(&#ty) -> &::std::string::String = |s| s;));
        }
    }

    // Reading: by name, missing members zero or empty.
    let mut from_dyn = Vec::new();
    let mut missing = Vec::new();
    for f in &fields {
        let (fi, ty, name) = (&f.ident, &f.ty, &f.name);
        let read_names: Vec<&str> = core::iter::once(name.as_str())
            .chain(f.aliases.iter().map(String::as_str))
            .collect();
        match &f.role {
            Role::Base => {
                let class_of = quote!(<#ty as _p::RootClass>::CLASS_NAME);
                from_dyn.push(quote!(#fi: _p::base::<#ty>(obj, #class_of)?,));
                missing.push(quote!(#fi: <#ty as _p::RootType>::__missing(),));
            }
            Role::Member | Role::StdString | Role::Counter => {
                from_dyn.push(quote!(#fi: _p::field::<#ty>(obj, &[#(#read_names),*])?,));
                missing.push(quote!(#fi: <#ty as _p::RootType>::__missing(),));
            }
            Role::Counted { item, .. } => {
                from_dyn.push(quote!(#fi: _p::counted::<#item>(obj, &[#(#read_names),*])?,));
                missing.push(quote!(#fi: _p::Vec::new(),));
            }
            Role::Skip => {
                from_dyn.push(quote!(#fi: ::core::default::Default::default(),));
                missing.push(quote!(#fi: ::core::default::Default::default(),));
            }
        }
    }

    Ok(quote! {
        #[doc(hidden)]
        #[allow(non_upper_case_globals, unused_attributes, unused_qualifications, clippy::all, clippy::pedantic)]
        const _: () = {
            #krate
            use _rhizoid::__private as _p;

            #[automatically_derived]
            impl _p::RootClass for #ident {
                const CLASS_NAME: &'static str = #class;
            }

            #[automatically_derived]
            impl _p::RootObject for #ident {
                const CLASS_VERSION: i16 = #version;
                const INHERITS_TOBJECT: bool = #(#tobject)||*;

                fn streamer_info() -> _p::Arc<_p::StreamerInfo> {
                    static INFO: _p::OnceLock<_p::Arc<_p::StreamerInfo>> = _p::OnceLock::new();
                    _p::Arc::clone(INFO.get_or_init(|| {
                        let mut elements = _p::Vec::new();
                        #(#elements)*
                        let mut bases: _p::Vec<_p::Arc<_p::StreamerInfo>> = _p::Vec::new();
                        #(#base_infos)*
                        _p::class_info(#class, #version, elements, &bases)
                    }))
                }

                fn __dependencies(out: &mut _p::Vec<_p::Arc<_p::StreamerInfo>>) {
                    let _ = &out;
                    #(#deps)*
                }
            }

            #[automatically_derived]
            impl _p::ToDyn for #ident {
                fn to_dyn(&self) -> _p::DynObject {
                    static DESCRIPTIONS: _p::OnceLock<_p::Descriptions> = _p::OnceLock::new();
                    let mut o = _p::object(#class, #version);
                    #(#to_dyn)*
                    _p::attach(&mut o, DESCRIPTIONS.get_or_init(_p::descriptions::<Self>));
                    o
                }
            }

            #[automatically_derived]
            impl _p::FromDyn for #ident {
                fn from_dyn(obj: &_p::DynObject) -> _rhizoid::Result<Self> {
                    _p::expect_class(obj, #class)?;
                    _p::Ok(Self { #(#from_dyn)* })
                }
            }

            #[automatically_derived]
            impl _p::RootType for #ident {
                fn __element(name: &str) -> _p::StreamerElement {
                    _p::object_element::<Self>(name)
                }
                fn __to_value(&self) -> _p::Value {
                    _p::object_value(self)
                }
                fn __from_value(v: &_p::Value) -> _rhizoid::Result<Self> {
                    _p::object_from(v, #class)
                }
                fn __missing() -> Self {
                    Self { #(#missing)* }
                }
                fn __dependencies(out: &mut _p::Vec<_p::Arc<_p::StreamerInfo>>) {
                    out.push(<Self as _p::RootObject>::streamer_info());
                    <Self as _p::RootObject>::__dependencies(out);
                }
            }

            #[automatically_derived]
            impl _p::VecItem for #ident {
                fn __item() -> (_p::String, i32) {
                    (_p::String::from(#class), 61)
                }
                fn __vec_value(v: &[Self]) -> _p::Value {
                    _p::objects_value(v)
                }
                fn __vec_from(v: &_p::Value) -> _rhizoid::Result<_p::Vec<Self>> {
                    _p::objects_from(v, #class)
                }
                fn __item_dependencies(out: &mut _p::Vec<_p::Arc<_p::StreamerInfo>>) {
                    <Self as _p::RootType>::__dependencies(out);
                }
            }

            #[automatically_derived]
            impl _p::BaseTitle for #ident {}

            #[automatically_derived]
            impl _p::RowField for #ident {
                type Column = _p::Value;
                fn __split(_: _p::Vec<_p::Value>) -> _p::Vec<Self> {
                    _p::Vec::new()
                }
                fn __try_split(out: _p::Vec<_p::Value>) -> _rhizoid::Result<_p::Vec<Self>> {
                    _p::objects_split(&out)
                }
                fn __shape() -> _p::Option<_rhizoid::tree::LeafShape> {
                    _p::Some(_p::object_shape(false))
                }
                fn __put(&self, out: &mut _rhizoid::tree::LeafBuffer) {
                    out.put_object(_p::Value::Object(_p::Box::new(_p::ToDyn::to_dyn(self))));
                }
                fn __class() -> _p::Option<_p::ClassSpec> {
                    _p::Some(_p::ClassSpec::of::<Self>())
                }
            }

            #[automatically_derived]
            impl _p::RowItem for #ident {
                type Column = _p::Value;
                fn __split_vecs(
                    out: _p::Vec<_p::Value>,
                ) -> _rhizoid::Result<_p::Vec<_p::Vec<Self>>> {
                    _p::object_lists_split(out)
                }
                fn __vec_shape() -> _p::Option<_rhizoid::tree::LeafShape> {
                    _p::Some(_p::object_shape(true))
                }
                fn __put_vec(v: &[Self], out: &mut _rhizoid::tree::LeafBuffer) {
                    out.put_object(_p::Value::List(
                        v.iter()
                            .map(|o| _p::Value::Object(_p::Box::new(_p::ToDyn::to_dyn(o))))
                            .collect(),
                    ));
                }
                fn __item_class() -> _p::Option<_p::ClassSpec> {
                    _p::Some(_p::ClassSpec::of::<Self>())
                }
            }
        };
    })
}

/// Derives `Row`: one field per column of a tree or an `RNTuple`.
///
/// Field attributes: `#[row(rename = "Column_name")]` (default: the
/// field's name); `#[row(count = "nMuon")]` for a `Vec` written to a tree,
/// the counter branch it is written with (default `n_` + its name; fields
/// naming the same counter share it, and must have as many values).
/// Container attribute `#[row(crate = "path")]`. Field types are
/// `RowField`s: numbers, `bool`, `[T; N]`, `Vec<T>` of numbers, `String`,
/// `Value` (read only).
#[proc_macro_derive(Row, attributes(row))]
pub fn derive_row(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match row(&input) {
        Ok(t) | Err(t) => t.into(),
    }
}

#[expect(clippy::too_many_lines, reason = "one generator, read top to bottom")]
fn row(input: &DeriveInput) -> Result<TokenStream2, TokenStream2> {
    let mut errors = Errors::default();
    let ident = &input.ident;
    if !input.generics.params.is_empty() {
        errors.push(
            input.generics.span(),
            "#[derive(Row)] does not support generics",
        );
    }
    let Data::Struct(data) = &input.data else {
        errors.push(ident.span(), "#[derive(Row)] needs a struct");
        errors.finish()?;
        return Err(TokenStream2::new());
    };
    let Fields::Named(named) = &data.fields else {
        errors.push(ident.span(), "#[derive(Row)] needs named fields");
        errors.finish()?;
        return Err(TokenStream2::new());
    };
    let mut krate = None::<LitStr>;
    for attr in input.attrs.iter().filter(|a| a.path().is_ident("row")) {
        let parsed = attr.parse_nested_meta(|m| {
            if m.path.is_ident("crate") {
                krate = Some(m.value()?.parse()?);
            } else {
                errors.push(m.path.span(), "unknown attribute: expected crate");
                skip_value(&m)?;
            }
            Ok(())
        });
        if let Err(e) = parsed {
            errors.0.push(e);
        }
    }
    let mut cols: Vec<(Ident, Type, String)> = Vec::new();
    let mut counts: Vec<Option<String>> = Vec::new();
    for f in &named.named {
        let Some(fi) = f.ident.clone() else { continue };
        let mut name = fi.to_string().trim_start_matches("r#").to_owned();
        let mut count = None;
        for attr in f.attrs.iter().filter(|a| a.path().is_ident("row")) {
            let parsed = attr.parse_nested_meta(|m| {
                if m.path.is_ident("rename") {
                    name = m.value()?.parse::<LitStr>()?.value();
                } else if m.path.is_ident("count") {
                    count = Some(m.value()?.parse::<LitStr>()?.value());
                } else {
                    errors.push(m.path.span(), "unknown attribute: expected rename or count");
                    skip_value(&m)?;
                }
                Ok(())
            });
            if let Err(e) = parsed {
                errors.0.push(e);
            }
        }
        if cols.iter().any(|(_, _, n)| *n == name) {
            errors.push(fi.span(), format!("two fields read the column `{name}`"));
        }
        cols.push((fi, f.ty.clone(), name));
        counts.push(count);
    }
    if cols.is_empty() {
        errors.push(ident.span(), "a row needs at least one field");
    }
    errors.finish()?;

    let krate = crate_tokens(krate.as_ref());
    let width = cols.len();
    let column_names = cols.iter().map(|(_, _, n)| n);
    let checks = cols
        .iter()
        .enumerate()
        .map(|(i, (_, ty, _))| quote!(#i => _p::row_check::<#ty>(interpretation),));
    let iters: Vec<Ident> = (0..width).map(|i| format_ident!("__f{i}")).collect();
    let takes = cols.iter().zip(&iters).map(|((_, ty, _), it)| {
        quote!(let mut #it = _p::row_field::<#ty>(columns.next().ok_or_else(_p::missing_column)?)?;)
    });
    let fills = cols.iter().zip(&iters).map(|((fi, _, _), it)| {
        quote!(#fi: match #it.next() { _p::Some(v) => v, _p::None => return _p::Ok(out) },)
    });
    let shapes = cols
        .iter()
        .map(|(_, ty, _)| quote!(<#ty as _p::RowField>::__shape()));
    let counts = counts.iter().map(|c| {
        c.as_ref()
            .map_or_else(|| quote!(_p::None), |c| quote!(_p::Some(#c)))
    });
    let classes = cols
        .iter()
        .map(|(_, ty, _)| quote!(<#ty as _p::RowField>::__class()));
    let puts = cols.iter().enumerate().map(|(i, (fi, ty, _))| {
        quote!(if let _p::Some(b) = out.get_mut(#i) { <#ty as _p::RowField>::__put(&self.#fi, b); })
    });
    Ok(quote! {
        #[doc(hidden)]
        #[allow(unused_qualifications, clippy::all, clippy::pedantic)]
        const _: () = {
            #krate
            use _rhizoid::__private as _p;

            #[automatically_derived]
            impl _p::Row for #ident {
                const __WIDTH: usize = #width;
                fn __names() -> _p::Vec<&'static str> {
                    _p::Vec::from([#(#column_names),*])
                }
                fn __check(i: usize, interpretation: &_p::Interpretation) -> _p::Result<(), _p::String> {
                    match i {
                        #(#checks)*
                        _ => _p::Err(_p::String::from("no such field")),
                    }
                }
                fn __rows(columns: _p::Vec<_p::Column>) -> _rhizoid::Result<_p::Vec<Self>> {
                    let mut columns = columns.into_iter();
                    #(#takes)*
                    let mut out = _p::Vec::new();
                    loop {
                        out.push(Self { #(#fills)* });
                    }
                }
                fn __shapes() -> _p::Vec<_p::Option<_p::LeafShape>> {
                    _p::Vec::from([#(#shapes),*])
                }
                fn __counts() -> _p::Vec<_p::Option<&'static str>> {
                    _p::Vec::from([#(#counts),*])
                }
                fn __classes() -> _p::Vec<_p::Option<_p::ClassSpec>> {
                    _p::Vec::from([#(#classes),*])
                }
                fn __put(&self, out: &mut [_p::LeafBuffer]) {
                    #(#puts)*
                }
            }
        };
    })
}
