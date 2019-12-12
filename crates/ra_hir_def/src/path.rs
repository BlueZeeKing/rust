//! A desugared representation of paths like `crate::foo` or `<Type as Trait>::bar`.
mod lower_use;

use std::{iter, sync::Arc};

use either::Either;
use hir_expand::{
    hygiene::Hygiene,
    name::{self, AsName, Name},
};
use ra_db::CrateId;
use ra_syntax::{
    ast::{self, TypeAscriptionOwner},
    AstNode,
};

use crate::{type_ref::TypeRef, InFile};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Path {
    pub kind: PathKind,
    pub segments: Vec<PathSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PathSegment {
    pub name: Name,
    pub args_and_bindings: Option<Arc<GenericArgs>>,
}

/// Generic arguments to a path segment (e.g. the `i32` in `Option<i32>`). This
/// can (in the future) also include bindings of associated types, like in
/// `Iterator<Item = Foo>`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GenericArgs {
    pub args: Vec<GenericArg>,
    /// This specifies whether the args contain a Self type as the first
    /// element. This is the case for path segments like `<T as Trait>`, where
    /// `T` is actually a type parameter for the path `Trait` specifying the
    /// Self type. Otherwise, when we have a path `Trait<X, Y>`, the Self type
    /// is left out.
    pub has_self_type: bool,
    /// Associated type bindings like in `Iterator<Item = T>`.
    pub bindings: Vec<(Name, TypeRef)>,
}

/// A single generic argument.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GenericArg {
    Type(TypeRef),
    // or lifetime...
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PathKind {
    Plain,
    Self_,
    Super,
    Crate,
    // Absolute path
    Abs,
    // Type based path like `<T>::foo`
    Type(Box<TypeRef>),
    // `$crate` from macro expansion
    DollarCrate(CrateId),
}

impl Path {
    /// Calls `cb` with all paths, represented by this use item.
    pub(crate) fn expand_use_item(
        item_src: InFile<ast::UseItem>,
        hygiene: &Hygiene,
        mut cb: impl FnMut(Path, &ast::UseTree, bool, Option<Name>),
    ) {
        if let Some(tree) = item_src.value.use_tree() {
            lower_use::lower_use_tree(None, tree, hygiene, &mut cb);
        }
    }

    pub(crate) fn from_simple_segments(
        kind: PathKind,
        segments: impl IntoIterator<Item = Name>,
    ) -> Path {
        Path {
            kind,
            segments: segments
                .into_iter()
                .map(|name| PathSegment { name, args_and_bindings: None })
                .collect(),
        }
    }

    /// Converts an `ast::Path` to `Path`. Works with use trees.
    /// DEPRECATED: It does not handle `$crate` from macro call.
    pub fn from_ast(path: ast::Path) -> Option<Path> {
        Path::from_src(path, &Hygiene::new_unhygienic())
    }

    /// Converts an `ast::Path` to `Path`. Works with use trees.
    /// It correctly handles `$crate` based path from macro call.
    pub fn from_src(mut path: ast::Path, hygiene: &Hygiene) -> Option<Path> {
        let mut kind = PathKind::Plain;
        let mut segments = Vec::new();
        loop {
            let segment = path.segment()?;

            if segment.has_colon_colon() {
                kind = PathKind::Abs;
            }

            match segment.kind()? {
                ast::PathSegmentKind::Name(name_ref) => {
                    // FIXME: this should just return name
                    match hygiene.name_ref_to_name(name_ref) {
                        Either::Left(name) => {
                            let args = segment
                                .type_arg_list()
                                .and_then(GenericArgs::from_ast)
                                .or_else(|| {
                                    GenericArgs::from_fn_like_path_ast(
                                        segment.param_list(),
                                        segment.ret_type(),
                                    )
                                })
                                .map(Arc::new);
                            let segment = PathSegment { name, args_and_bindings: args };
                            segments.push(segment);
                        }
                        Either::Right(crate_id) => {
                            kind = PathKind::DollarCrate(crate_id);
                            break;
                        }
                    }
                }
                ast::PathSegmentKind::Type { type_ref, trait_ref } => {
                    assert!(path.qualifier().is_none()); // this can only occur at the first segment

                    let self_type = TypeRef::from_ast(type_ref?);

                    match trait_ref {
                        // <T>::foo
                        None => {
                            kind = PathKind::Type(Box::new(self_type));
                        }
                        // <T as Trait<A>>::Foo desugars to Trait<Self=T, A>::Foo
                        Some(trait_ref) => {
                            let path = Path::from_src(trait_ref.path()?, hygiene)?;
                            kind = path.kind;
                            let mut prefix_segments = path.segments;
                            prefix_segments.reverse();
                            segments.extend(prefix_segments);
                            // Insert the type reference (T in the above example) as Self parameter for the trait
                            let mut last_segment = segments.last_mut()?;
                            if last_segment.args_and_bindings.is_none() {
                                last_segment.args_and_bindings =
                                    Some(Arc::new(GenericArgs::empty()));
                            };
                            let args = last_segment.args_and_bindings.as_mut().unwrap();
                            let mut args_inner = Arc::make_mut(args);
                            args_inner.has_self_type = true;
                            args_inner.args.insert(0, GenericArg::Type(self_type));
                        }
                    }
                }
                ast::PathSegmentKind::CrateKw => {
                    kind = PathKind::Crate;
                    break;
                }
                ast::PathSegmentKind::SelfKw => {
                    kind = PathKind::Self_;
                    break;
                }
                ast::PathSegmentKind::SuperKw => {
                    kind = PathKind::Super;
                    break;
                }
            }
            path = match qualifier(&path) {
                Some(it) => it,
                None => break,
            };
        }
        segments.reverse();
        return Some(Path { kind, segments });

        fn qualifier(path: &ast::Path) -> Option<ast::Path> {
            if let Some(q) = path.qualifier() {
                return Some(q);
            }
            // FIXME: this bottom up traversal is not too precise.
            // Should we handle do a top-down analysis, recording results?
            let use_tree_list = path.syntax().ancestors().find_map(ast::UseTreeList::cast)?;
            let use_tree = use_tree_list.parent_use_tree();
            use_tree.path()
        }
    }

    /// Converts an `ast::NameRef` into a single-identifier `Path`.
    pub(crate) fn from_name_ref(name_ref: &ast::NameRef) -> Path {
        name_ref.as_name().into()
    }

    /// Converts an `tt::Ident` into a single-identifier `Path`.
    pub(crate) fn from_tt_ident(ident: &tt::Ident) -> Path {
        ident.as_name().into()
    }

    /// `true` is this path is a single identifier, like `foo`
    pub fn is_ident(&self) -> bool {
        self.kind == PathKind::Plain && self.segments.len() == 1
    }

    /// `true` if this path is just a standalone `self`
    pub fn is_self(&self) -> bool {
        self.kind == PathKind::Self_ && self.segments.is_empty()
    }

    /// If this path is a single identifier, like `foo`, return its name.
    pub fn as_ident(&self) -> Option<&Name> {
        if self.kind != PathKind::Plain || self.segments.len() > 1 {
            return None;
        }
        self.segments.first().map(|s| &s.name)
    }

    pub fn expand_macro_expr(&self) -> Option<Name> {
        self.as_ident().and_then(|name| Some(name.clone()))
    }

    pub fn is_type_relative(&self) -> bool {
        match self.kind {
            PathKind::Type(_) => true,
            _ => false,
        }
    }
}

impl GenericArgs {
    pub(crate) fn from_ast(node: ast::TypeArgList) -> Option<GenericArgs> {
        let mut args = Vec::new();
        for type_arg in node.type_args() {
            let type_ref = TypeRef::from_ast_opt(type_arg.type_ref());
            args.push(GenericArg::Type(type_ref));
        }
        // lifetimes ignored for now
        let mut bindings = Vec::new();
        for assoc_type_arg in node.assoc_type_args() {
            if let Some(name_ref) = assoc_type_arg.name_ref() {
                let name = name_ref.as_name();
                let type_ref = TypeRef::from_ast_opt(assoc_type_arg.type_ref());
                bindings.push((name, type_ref));
            }
        }
        if args.is_empty() && bindings.is_empty() {
            None
        } else {
            Some(GenericArgs { args, has_self_type: false, bindings })
        }
    }

    /// Collect `GenericArgs` from the parts of a fn-like path, i.e. `Fn(X, Y)
    /// -> Z` (which desugars to `Fn<(X, Y), Output=Z>`).
    pub(crate) fn from_fn_like_path_ast(
        params: Option<ast::ParamList>,
        ret_type: Option<ast::RetType>,
    ) -> Option<GenericArgs> {
        let mut args = Vec::new();
        let mut bindings = Vec::new();
        if let Some(params) = params {
            let mut param_types = Vec::new();
            for param in params.params() {
                let type_ref = TypeRef::from_ast_opt(param.ascribed_type());
                param_types.push(type_ref);
            }
            let arg = GenericArg::Type(TypeRef::Tuple(param_types));
            args.push(arg);
        }
        if let Some(ret_type) = ret_type {
            let type_ref = TypeRef::from_ast_opt(ret_type.type_ref());
            bindings.push((name::OUTPUT_TYPE, type_ref))
        }
        if args.is_empty() && bindings.is_empty() {
            None
        } else {
            Some(GenericArgs { args, has_self_type: false, bindings })
        }
    }

    pub(crate) fn empty() -> GenericArgs {
        GenericArgs { args: Vec::new(), has_self_type: false, bindings: Vec::new() }
    }
}

impl From<Name> for Path {
    fn from(name: Name) -> Path {
        Path::from_simple_segments(PathKind::Plain, iter::once(name))
    }
}

pub mod known {
    use hir_expand::name;

    use super::{Path, PathKind};

    pub fn std_iter_into_iterator() -> Path {
        Path::from_simple_segments(
            PathKind::Abs,
            vec![name::STD, name::ITER, name::INTO_ITERATOR_TYPE],
        )
    }

    pub fn std_ops_try() -> Path {
        Path::from_simple_segments(PathKind::Abs, vec![name::STD, name::OPS, name::TRY_TYPE])
    }

    pub fn std_ops_range() -> Path {
        Path::from_simple_segments(PathKind::Abs, vec![name::STD, name::OPS, name::RANGE_TYPE])
    }

    pub fn std_ops_range_from() -> Path {
        Path::from_simple_segments(PathKind::Abs, vec![name::STD, name::OPS, name::RANGE_FROM_TYPE])
    }

    pub fn std_ops_range_full() -> Path {
        Path::from_simple_segments(PathKind::Abs, vec![name::STD, name::OPS, name::RANGE_FULL_TYPE])
    }

    pub fn std_ops_range_inclusive() -> Path {
        Path::from_simple_segments(
            PathKind::Abs,
            vec![name::STD, name::OPS, name::RANGE_INCLUSIVE_TYPE],
        )
    }

    pub fn std_ops_range_to() -> Path {
        Path::from_simple_segments(PathKind::Abs, vec![name::STD, name::OPS, name::RANGE_TO_TYPE])
    }

    pub fn std_ops_range_to_inclusive() -> Path {
        Path::from_simple_segments(
            PathKind::Abs,
            vec![name::STD, name::OPS, name::RANGE_TO_INCLUSIVE_TYPE],
        )
    }

    pub fn std_result_result() -> Path {
        Path::from_simple_segments(PathKind::Abs, vec![name::STD, name::RESULT, name::RESULT_TYPE])
    }

    pub fn std_future_future() -> Path {
        Path::from_simple_segments(PathKind::Abs, vec![name::STD, name::FUTURE, name::FUTURE_TYPE])
    }

    pub fn std_boxed_box() -> Path {
        Path::from_simple_segments(PathKind::Abs, vec![name::STD, name::BOXED, name::BOX_TYPE])
    }
}
