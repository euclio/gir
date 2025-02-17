use crate::{
    analysis::{
        function_parameters::{async_param_to_remove, CParameter},
        functions::{find_function, find_index_to_ignore, finish_function_name},
        imports::Imports,
        out_parameters::use_function_return_for_result,
        ref_mode::RefMode,
        rust_type::RustType,
    },
    config,
    consts::TYPE_PARAMETERS_START,
    env::Env,
    library::{
        Class, Concurrency, Function, Fundamental, Nullable, ParameterDirection, Type, TypeId,
    },
    traits::IntoString,
};
use std::{collections::vec_deque::VecDeque, slice::Iter};

#[derive(Clone, Eq, Debug, PartialEq)]
pub enum BoundType {
    NoWrapper,
    // lifetime
    IsA(Option<char>),
    // lifetime <- shouldn't be used but just in case...
    AsRef(Option<char>),
}

impl BoundType {
    pub fn need_isa(&self) -> bool {
        matches!(*self, BoundType::IsA(_))
    }
}

#[derive(Clone, Eq, Debug, PartialEq)]
pub struct Bound {
    pub bound_type: BoundType,
    pub parameter_name: String,
    pub alias: char,
    pub type_str: String,
    pub info_for_next_type: bool,
    pub callback_modified: bool,
}

#[derive(Clone, Debug)]
pub struct Bounds {
    unused: VecDeque<char>,
    used: Vec<Bound>,
    unused_lifetimes: VecDeque<char>,
    lifetimes: Vec<char>,
}

impl Default for Bounds {
    fn default() -> Bounds {
        Bounds {
            unused: (TYPE_PARAMETERS_START as u8..)
                .take_while(|x| *x <= b'Z')
                .map(|x| x as char)
                .collect(),
            used: Vec::new(),
            unused_lifetimes: "abcdefg".chars().collect(),
            lifetimes: Vec::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct CallbackInfo {
    pub callback_type: String,
    pub success_parameters: String,
    pub error_parameters: String,
    pub bound_name: char,
}

impl Bounds {
    pub fn add_for_parameter(
        &mut self,
        env: &Env,
        func: &Function,
        par: &CParameter,
        r#async: bool,
        concurrency: Concurrency,
        configured_functions: &[&config::functions::Function],
    ) -> (Option<String>, Option<CallbackInfo>) {
        let type_name = RustType::builder(env, par.typ)
            .with_ref_mode(RefMode::ByRefFake)
            .try_build();
        if (r#async && async_param_to_remove(&par.name)) || type_name.is_err() {
            return (None, None);
        }
        let mut type_string = type_name.into_string();
        let mut callback_info = None;
        let mut ret = None;
        let mut need_is_into_check = false;

        if !par.instance_parameter && par.direction != ParameterDirection::Out {
            if let Some(bound_type) = Bounds::type_for(env, par.typ, par.nullable) {
                ret = Some(Bounds::get_to_glib_extra(&bound_type));
                if r#async && (par.name == "callback" || par.name.ends_with("_callback")) {
                    let func_name = func.c_identifier.as_ref().unwrap();
                    let finish_func_name = finish_function_name(func_name);
                    if let Some(function) = find_function(env, &finish_func_name) {
                        // FIXME: This should work completely based on the analysis of the finish() function
                        // but that a) happens afterwards and b) is not accessible from here either.
                        let mut out_parameters =
                            find_out_parameters(env, function, configured_functions);
                        if use_function_return_for_result(
                            env,
                            function.ret.typ,
                            &func.name,
                            configured_functions,
                        ) {
                            out_parameters.insert(
                                0,
                                RustType::builder(env, function.ret.typ)
                                    .with_direction(function.ret.direction)
                                    .with_nullable(function.ret.nullable)
                                    .try_build()
                                    .into_string(),
                            );
                        }
                        let parameters = format_out_parameters(&out_parameters);
                        let error_type = find_error_type(env, function);
                        type_string = format!(
                            "FnOnce(Result<{}, {}>) + Send + 'static",
                            parameters, error_type
                        );
                        let bound_name = *self.unused.front().unwrap();
                        callback_info = Some(CallbackInfo {
                            callback_type: type_string.clone(),
                            success_parameters: parameters,
                            error_parameters: error_type,
                            bound_name,
                        });
                    }
                } else if par.c_type == "GDestroyNotify" || env.library.type_(par.typ).is_function()
                {
                    need_is_into_check = par.c_type != "GDestroyNotify";
                    if let Type::Function(_) = env.library.type_(par.typ) {
                        type_string = RustType::builder(env, par.typ)
                            .with_direction(par.direction)
                            .with_scope(par.scope)
                            .with_concurrency(concurrency)
                            .with_try_from_glib(&par.try_from_glib)
                            .try_build()
                            .into_string();
                        let bound_name = *self.unused.front().unwrap();
                        callback_info = Some(CallbackInfo {
                            callback_type: type_string.clone(),
                            success_parameters: String::new(),
                            error_parameters: String::new(),
                            bound_name,
                        });
                    }
                }
                if (!need_is_into_check || !*par.nullable)
                    && par.c_type != "GDestroyNotify"
                    && !self.add_parameter(&par.name, &type_string, bound_type, r#async)
                {
                    panic!(
                        "Too many type constraints for {}",
                        func.c_identifier.as_ref().unwrap()
                    )
                }
            }
        } else if par.instance_parameter {
            if let Some(bound_type) = Bounds::type_for(env, par.typ, par.nullable) {
                ret = Some(Bounds::get_to_glib_extra(&bound_type));
            }
        }

        (ret, callback_info)
    }

    pub fn type_for(env: &Env, type_id: TypeId, nullable: Nullable) -> Option<BoundType> {
        use self::BoundType::*;
        match *env.library.type_(type_id) {
            Type::Fundamental(Fundamental::Filename) => Some(AsRef(None)),
            Type::Fundamental(Fundamental::OsString) => Some(AsRef(None)),
            Type::Fundamental(Fundamental::Utf8) if *nullable => None,
            Type::Class(Class { final_type, .. }) => {
                if final_type {
                    None
                } else {
                    Some(IsA(None))
                }
            }
            Type::Interface(..) => Some(IsA(None)),
            Type::List(_) | Type::SList(_) | Type::CArray(_) => None,
            Type::Fundamental(_) if *nullable => None,
            Type::Function(_) => Some(NoWrapper),
            _ => None,
        }
    }

    fn get_to_glib_extra(bound_type: &BoundType) -> String {
        use self::BoundType::*;
        match *bound_type {
            AsRef(_) => ".as_ref()".to_owned(),
            IsA(_) => ".as_ref()".to_owned(),
            _ => String::new(),
        }
    }

    pub fn add_parameter(
        &mut self,
        name: &str,
        type_str: &str,
        bound_type: BoundType,
        r#async: bool,
    ) -> bool {
        if r#async && name == "callback" {
            if let Some(alias) = self.unused.pop_front() {
                self.used.push(Bound {
                    bound_type: BoundType::NoWrapper,
                    parameter_name: name.to_owned(),
                    alias,
                    type_str: type_str.to_string(),
                    info_for_next_type: false,
                    callback_modified: false,
                });
                return true;
            }
            return false;
        }
        if self.used.iter().any(|n| n.parameter_name == name) {
            return false;
        }
        if let Some(alias) = self.unused.pop_front() {
            self.used.push(Bound {
                bound_type,
                parameter_name: name.to_owned(),
                alias,
                type_str: type_str.to_owned(),
                info_for_next_type: false,
                callback_modified: false,
            });
            true
        } else {
            false
        }
    }

    pub fn get_parameter_alias_info(&self, name: &str) -> Option<(char, BoundType)> {
        self.used
            .iter()
            .find(move |n| {
                if n.parameter_name == name {
                    !n.info_for_next_type
                } else {
                    false
                }
            })
            .map(|t| (t.alias, t.bound_type.clone()))
    }

    pub fn get_base_alias(&self, alias: char) -> Option<char> {
        if alias == TYPE_PARAMETERS_START {
            return None;
        }
        let prev_alias = ((alias as u8) - 1) as char;
        self.used
            .iter()
            .find(move |n| n.alias == prev_alias)
            .and_then(|b| {
                if b.info_for_next_type {
                    Some(b.alias)
                } else {
                    None
                }
            })
    }

    pub fn update_imports(&self, imports: &mut Imports) {
        //TODO: import with versions
        use self::BoundType::*;
        for used in &self.used {
            match used.bound_type {
                NoWrapper => (),
                IsA(_) => imports.add("glib::object::IsA"),
                AsRef(_) => imports.add_used_type(&used.type_str),
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        self.used.is_empty()
    }

    pub fn iter(&self) -> Iter<'_, Bound> {
        self.used.iter()
    }

    pub fn iter_lifetimes(&self) -> Iter<'_, char> {
        self.lifetimes.iter()
    }
}

#[derive(Clone, Debug)]
pub struct PropertyBound {
    pub alias: char,
    pub type_str: String,
}

impl PropertyBound {
    pub fn get(env: &Env, type_id: TypeId) -> Option<PropertyBound> {
        let type_ = env.type_(type_id);
        if type_.is_final_type() {
            return None;
        }
        Some(PropertyBound {
            alias: TYPE_PARAMETERS_START,
            type_str: RustType::builder(env, type_id)
                .with_ref_mode(RefMode::ByRefFake)
                .try_build()
                .into_string(),
        })
    }
}

fn find_out_parameters(
    env: &Env,
    function: &Function,
    configured_functions: &[&config::functions::Function],
) -> Vec<String> {
    let index_to_ignore = find_index_to_ignore(&function.parameters, Some(&function.ret));
    function
        .parameters
        .iter()
        .enumerate()
        .filter(|&(index, param)| {
            Some(index) != index_to_ignore
                && param.direction == ParameterDirection::Out
                && param.name != "error"
        })
        .map(|(_, param)| {
            // FIXME: This should work completely based on the analysis of the finish() function
            // but that a) happens afterwards and b) is not accessible from here either.
            let nullable = configured_functions
                .iter()
                .find_map(|f| f.parameters.iter().find_map(|p| p.nullable))
                .unwrap_or(param.nullable);

            RustType::builder(env, param.typ)
                .with_direction(param.direction)
                .with_nullable(nullable)
                .try_build()
                .into_string()
        })
        .collect()
}

fn format_out_parameters(parameters: &[String]) -> String {
    if parameters.len() == 1 {
        parameters[0].to_string()
    } else {
        format!("({})", parameters.join(", "))
    }
}

fn find_error_type(env: &Env, function: &Function) -> String {
    let error_param = function
        .parameters
        .iter()
        .find(|param| param.direction == ParameterDirection::Out && param.name == "error")
        .expect("error type");
    if let Type::Record(_) = *env.type_(error_param.typ) {
        return RustType::builder(env, error_param.typ)
            .with_direction(error_param.direction)
            .try_build()
            .into_string();
    }
    panic!("cannot find error type")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_new_all() {
        let mut bounds: Bounds = Default::default();
        let typ = BoundType::IsA(None);
        assert_eq!(bounds.add_parameter("a", "", typ.clone(), false), true);
        // Don't add second time
        assert_eq!(bounds.add_parameter("a", "", typ.clone(), false), false);
        assert_eq!(bounds.add_parameter("b", "", typ.clone(), false), true);
        assert_eq!(bounds.add_parameter("c", "", typ.clone(), false), true);
        assert_eq!(bounds.add_parameter("d", "", typ.clone(), false), true);
        assert_eq!(bounds.add_parameter("e", "", typ.clone(), false), true);
        assert_eq!(bounds.add_parameter("f", "", typ.clone(), false), true);
        assert_eq!(bounds.add_parameter("g", "", typ.clone(), false), true);
        assert_eq!(bounds.add_parameter("h", "", typ.clone(), false), true);
        assert_eq!(bounds.add_parameter("h", "", typ.clone(), false), false);
        assert_eq!(bounds.add_parameter("i", "", typ.clone(), false), true);
        assert_eq!(bounds.add_parameter("j", "", typ.clone(), false), true);
        assert_eq!(bounds.add_parameter("k", "", typ.clone(), false), true);
        assert_eq!(bounds.add_parameter("l", "", typ, false), false);
    }

    #[test]
    fn get_parameter_alias_info() {
        let mut bounds: Bounds = Default::default();
        let typ = BoundType::IsA(None);
        bounds.add_parameter("a", "", typ.clone(), false);
        bounds.add_parameter("b", "", typ.clone(), false);
        assert_eq!(
            bounds.get_parameter_alias_info("a"),
            Some(('P', typ.clone()))
        );
        assert_eq!(bounds.get_parameter_alias_info("b"), Some(('Q', typ)));
        assert_eq!(bounds.get_parameter_alias_info("c"), None);
    }
}
